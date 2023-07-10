use std::fmt::Display;
use std::io::Cursor;

use anyhow::bail;
use anyhow::Error;
use anyhow::Result;
use get_size::GetSize;
use serde_derive::Deserialize;
use serde_derive::Serialize;
use twenty_first::shared_math::b_field_element::BFieldElement;
use twenty_first::shared_math::bfield_codec::BFieldCodec;
use twenty_first::shared_math::digest::Digest;
use twenty_first::util_types::algebraic_hasher::AlgebraicHasher;

use crate::aet::AlgebraicExecutionTrace;
use crate::error::InstructionError::InstructionPointerOverflow;
use crate::instruction::convert_labels;
use crate::instruction::Instruction;
use crate::instruction::LabelledInstruction;
use crate::parser::parse;
use crate::parser::to_labelled;
use crate::vm::VMState;

/// A `Program` is a `Vec<Instruction>` that contains duplicate elements for instructions with a
/// size of 2. This means that the index in the vector corresponds to the VM's
/// `instruction_pointer`. These duplicate values should most often be skipped/ignored,
/// e.g. when pretty-printing.
#[derive(Debug, Clone, Default, PartialEq, Eq, GetSize, Serialize, Deserialize)]
pub struct Program {
    pub instructions: Vec<Instruction>,
}

impl Display for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut stream = self.instructions.iter();
        while let Some(instruction) = stream.next() {
            writeln!(f, "{instruction}")?;
            // 2-word instructions already print their arguments
            for _ in 1..instruction.size() {
                stream.next();
            }
        }
        Ok(())
    }
}

impl BFieldCodec for Program {
    fn decode(sequence: &[BFieldElement]) -> Result<Box<Self>> {
        if sequence.is_empty() {
            bail!("Sequence to decode must not be empty.");
        }
        let program_length = sequence[0].value() as usize;
        let sequence = &sequence[1..];
        if sequence.len() != program_length {
            bail!(
                "Sequence to decode must have length {program_length}, but has length {}.",
                sequence.len()
            );
        }

        let mut idx = 0;
        let mut instructions = Vec::with_capacity(program_length);
        while idx < program_length {
            let opcode: u32 = match sequence[idx].value().try_into() {
                Ok(opcode) => opcode,
                Err(_) => bail!("Invalid opcode {} at index {idx}.", sequence[idx].value()),
            };
            let instruction: Instruction = opcode.try_into()?;
            if !instruction.has_arg() {
                instructions.push(instruction);
            } else if instructions.len() + 1 >= program_length {
                bail!("Missing argument for instruction {instruction} at index {idx}.");
            } else {
                let instruction = match instruction.change_arg(sequence[idx + 1]) {
                    Some(instruction) => instruction,
                    None => {
                        bail!("Invalid argument for instruction {instruction} at index {idx}.")
                    }
                };
                // Instructions with argument are recorded twice to align the `instruction_pointer`.
                instructions.push(instruction);
                instructions.push(instruction);
            }
            idx += instruction.size();
        }

        if idx != program_length {
            bail!("Decoded program must have length {program_length}, but has length {idx}.",);
        }
        Ok(Box::new(Program { instructions }))
    }

    fn encode(&self) -> Vec<BFieldElement> {
        let mut sequence = Vec::with_capacity(self.len_bwords() + 1);
        sequence.push(BFieldElement::new(self.len_bwords() as u64));
        sequence.extend(self.to_bwords());
        sequence
    }

    fn static_length() -> Option<usize> {
        None
    }
}

/// An `InstructionIter` loops the instructions of a `Program` by skipping duplicate placeholders.
pub struct InstructionIter {
    cursor: Cursor<Vec<Instruction>>,
}

impl Iterator for InstructionIter {
    type Item = Instruction;

    fn next(&mut self) -> Option<Self::Item> {
        let pos = self.cursor.position() as usize;
        let instructions = self.cursor.get_ref();
        let instruction = *instructions.get(pos)?;
        self.cursor.set_position((pos + instruction.size()) as u64);

        Some(instruction)
    }
}

impl IntoIterator for Program {
    type Item = Instruction;

    type IntoIter = InstructionIter;

    fn into_iter(self) -> Self::IntoIter {
        let cursor = Cursor::new(self.instructions);
        InstructionIter { cursor }
    }
}

impl Program {
    /// Create a `Program` from a slice of `Instruction`.
    pub fn new(input: &[LabelledInstruction]) -> Self {
        let instructions = convert_labels(input)
            .iter()
            .flat_map(|instr| vec![*instr; instr.size()])
            .collect::<Vec<_>>();

        Program { instructions }
    }

    /// Create a `Program` by parsing source code.
    pub fn from_code(code: &str) -> Result<Self> {
        parse(code)
            .map(|program| Program::new(&to_labelled(&program)))
            .map_err(|err| anyhow::anyhow!("{}", err))
    }

    /// Convert a `Program` to a `Vec<BFieldElement>`.
    ///
    /// Every single-word instruction is converted to a single word.
    ///
    /// Every double-word instruction is converted to two words.
    pub fn to_bwords(&self) -> Vec<BFieldElement> {
        self.clone()
            .into_iter()
            .flat_map(|instruction| {
                let opcode = instruction.opcode_b();
                if let Some(arg) = instruction.arg() {
                    vec![opcode, arg]
                } else {
                    vec![opcode]
                }
            })
            .collect()
    }

    /// The total length of the program as `BFieldElement`s. Double-word instructions contribute
    /// two `BFieldElement`s.
    pub fn len_bwords(&self) -> usize {
        self.instructions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// Hash the program using the given `AlgebraicHasher`.
    pub fn hash<H: AlgebraicHasher>(&self) -> Digest {
        H::hash_varlen(&self.to_bwords())
    }

    /// Run Triton VM on the given [`Program`] with the given public and secret input.
    ///
    /// See also [`trace_execution`](Self::trace_execution) and [`debug`](Self::debug).
    pub fn run(
        &self,
        public_input: Vec<BFieldElement>,
        secret_input: Vec<BFieldElement>,
    ) -> Result<Vec<BFieldElement>> {
        let mut state = VMState::new(self, public_input, secret_input);
        while !state.halting {
            state.step()?;
        }
        Ok(state.public_output)
    }

    /// Trace the execution of a [`Program`]. That is, [`run`][run] the [`Program`] and additionally
    /// record that part of every encountered state that is necessary for proving correct execution.
    /// If execution  succeeds, returns
    /// 1. an [`AlgebraicExecutionTrace`], and
    /// 1. the output of the program.
    ///
    /// See also [`debug`](Self::debug) and [`run`][run].
    ///
    /// [run]: Self::run
    pub fn trace_execution(
        &self,
        public_input: Vec<BFieldElement>,
        secret_input: Vec<BFieldElement>,
    ) -> Result<(AlgebraicExecutionTrace, Vec<BFieldElement>)> {
        let mut aet = AlgebraicExecutionTrace::new(self.clone());
        let mut state = VMState::new(self, public_input, secret_input);
        assert_eq!(self.len_bwords(), aet.instruction_multiplicities.len());

        while !state.halting {
            aet.record_state(&state)?;

            match state.instruction_pointer < aet.instruction_multiplicities.len() {
                true => aet.instruction_multiplicities[state.instruction_pointer] += 1,
                false => bail!(InstructionPointerOverflow(state.instruction_pointer)),
            }

            let maybe_co_processor_call = state.step()?;
            if let Some(co_processor_call) = maybe_co_processor_call {
                aet.record_co_processor_call(co_processor_call);
            }
        }

        Ok((aet, state.public_output))
    }

    /// Similar to [`run`](Self::run), but also returns a [`Vec`] of [`VMState`]s, one for each
    /// step of the VM. On premature termination of the VM, returns all [`VMState`]s up to the
    /// point of failure.
    ///
    /// The VM's initial state is either the provided `initial_state`, or a new [`VMState`] if
    /// `initial_state` is `None`. The initial state is included in the returned [`Vec`] of
    /// [`VMState`]s. If an initial state is provided, the `program`, `public_input` and
    /// `private_input` provided to this method are ignored and the initial state's program and
    /// inputs are used instead.
    ///
    /// If `num_cycles_to_execute` is `Some(number_of_cycles)`, the VM will execute at most
    /// `number_of_cycles` cycles. If `num_cycles_to_execute` is `None`, the VM will execute until
    /// it halts or the maximum number of cycles (2^{32}) is reached.
    ///
    /// See also [`debug_terminal_state`](Self::debug_terminal_state) and
    /// [`trace_execution`](Self::trace_execution).
    pub fn debug<'pgm>(
        &'pgm self,
        public_input: Vec<BFieldElement>,
        secret_input: Vec<BFieldElement>,
        initial_state: Option<VMState<'pgm>>,
        num_cycles_to_execute: Option<u32>,
    ) -> (Vec<VMState<'pgm>>, Option<Error>) {
        let mut states = vec![];
        let mut state = match initial_state {
            Some(initial_state) => initial_state,
            None => VMState::new(self, public_input, secret_input),
        };

        let max_cycles = match num_cycles_to_execute {
            Some(number_of_cycles) => state.cycle_count + number_of_cycles,
            None => u32::MAX,
        };

        while !state.halting && state.cycle_count < max_cycles {
            states.push(state.clone());
            if let Err(err) = state.step() {
                return (states, Some(err));
            }
        }

        states.push(state);
        (states, None)
    }

    /// Run Triton VM on the given [`Program`] with the given public and secret input, and return
    /// the final [`VMState`]. Requires substantially less RAM than [`debug`][debug] since no
    /// intermediate states are recorded.
    ///
    /// Parameters `initial_state` and `num_cycles_to_execute` are handled like in [`debug`][debug];
    /// see there for more details.
    ///
    /// If an error is encountered, returns the error and the [`VMState`] at the point of failure.
    ///
    /// See also [`trace_execution`](Self::trace_execution) and [`run`](Self::run).
    ///
    /// [debug]: Self::debug
    pub fn debug_terminal_state<'pgm>(
        &'pgm self,
        public_input: Vec<BFieldElement>,
        secret_input: Vec<BFieldElement>,
        initial_state: Option<VMState<'pgm>>,
        num_cycles_to_execute: Option<u32>,
    ) -> Result<VMState<'pgm>, (Error, VMState<'pgm>)> {
        let mut state = match initial_state {
            Some(initial_state) => initial_state,
            None => VMState::new(self, public_input, secret_input),
        };

        let max_cycles = match num_cycles_to_execute {
            Some(number_of_cycles) => state.cycle_count + number_of_cycles,
            None => u32::MAX,
        };

        while !state.halting && state.cycle_count < max_cycles {
            // The internal state transition method [`VMState::step`] is not atomic.
            // To avoid returning an inconsistent state in case of a failed transition, the last
            // known-to-be-consistent state is returned.
            let previous_state = state.clone();
            if let Err(err) = state.step() {
                return Err((err, previous_state));
            }
        }
        Ok(state)
    }
}

#[cfg(test)]
mod test {
    use rand::thread_rng;
    use rand::Rng;
    use twenty_first::shared_math::tip5::Tip5;

    use crate::parser::parser_tests::program_gen;

    use super::*;

    #[test]
    fn random_program_encode_decode_equivalence() {
        let mut rng = thread_rng();
        for _ in 0..50 {
            let program_len = rng.gen_range(20..420);
            let source_code = program_gen(program_len);
            let program = Program::from_code(&source_code).unwrap();

            let encoded = program.encode();
            let decoded = *Program::decode(&encoded).unwrap();

            assert_eq!(program, decoded);
        }
    }

    #[test]
    fn decode_program_with_missing_argument_as_last_instruction() {
        let program = Program::from_code("push 3 push 3 eq assert push 3").unwrap();
        let program_length = program.len_bwords() as u64;
        let encoded = program.encode();

        let mut encoded = encoded[0..encoded.len() - 1].to_vec();
        encoded[0] = BFieldElement::new(program_length - 1);

        let err = Program::decode(&encoded).err().unwrap();
        assert_eq!(
            "Missing argument for instruction push 0 at index 6.",
            err.to_string(),
        );
    }

    #[test]
    fn decode_program_with_length_mismatch() {
        let program = Program::from_code("nop nop hash push 0 skiz end: halt call end").unwrap();
        let program_length = program.len_bwords() as u64;
        let mut encoded = program.encode();

        encoded[0] = BFieldElement::new(program_length + 1);

        let err = Program::decode(&encoded).err().unwrap();
        assert_eq!(
            "Sequence to decode must have length 10, but has length 9.",
            err.to_string(),
        );
    }

    #[test]
    fn decode_program_from_empty_sequence() {
        let encoded = vec![];
        let err = Program::decode(&encoded).err().unwrap();
        assert_eq!("Sequence to decode must not be empty.", err.to_string(),);
    }

    #[test]
    fn hash_simple_program() {
        let program = Program::from_code("halt").unwrap();
        let digest = program.hash::<Tip5>();

        let expected_digest = [
            4843866011885844809,
            16618866032559590857,
            18247689143239181392,
            7637465675240023996,
            9104890367162237026,
        ]
        .map(BFieldElement::new);
        let expected_digest = Digest::new(expected_digest);

        assert_eq!(expected_digest, digest);
    }

    #[test]
    fn empty_program_is_empty() {
        let program = Program::from_code("").unwrap();
        assert!(program.is_empty());
    }
}
