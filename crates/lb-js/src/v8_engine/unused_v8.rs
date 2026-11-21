//! V8 compiler paths that are off in every LightBrowser process, kept out
//! of the binary (M6 binary size, ADR 0013).
//!
//! Each function here has the mangled name of the only strong global
//! symbol (or symbols) of one object file in the prebuilt V8 archive.
//! When the linker reads the archive, the symbol is already defined, so
//! that object is never extracted, the same mechanism as `icudt78_dat` in
//! [`super::icu_data`]. If anything else in the binary needed another
//! symbol of such an object, the linker would extract it and fail with a
//! duplicate-symbol error, so this cannot drop code silently. Together
//! they take about 3.4 MB off the release binary.
//!
//! Every path below runs only under a V8 flag that is off by default, and
//! LightBrowser never turns one on: V8 gets no flags from the command line
//! or the page, [`super::init_v8`] refuses the ones that would reach these
//! functions, and V8 freezes its flags after initialisation. A call is
//! therefore a bug; the stubs abort with the name of the path.
//!
//! | object | gated by | linked size before |
//! |---|---|---|
//! | `turbolev-graph-builder.o`, `turbolev-frontend-pipeline.o` | `--turbolev` (Turbolev instead of Turbofan) | 585 + 234 KB |
//! | `wasm-in-js-inlining-phase.o` | a Turbolev graph with inlineable Wasm calls | 356 KB |
//! | `type-assertions-phase.o` | `--turboshaft-assert-types` | 359 KB |
//! | `typed-optimizations-phase.o` | `--turboshaft-typed-optimizations` | 262 KB |
//! | `loop-optimization-phase.o` | `--turboshaft-loop-optimization` | 215 KB |
//! | `random-rescheduling-phase.o` | `--turboshaft-random-rescheduling`, `--wasm-random-rescheduling` | 145 KB |
//! | `block-instrumentation-phase.o` | `--turbo-profiling` while compiling builtins (mksnapshot) | 279 KB |
//! | `wasm-revec-phase.o` | `--wasm-revectorize` (alias `--experimental-wasm-revectorize`) | 338 KB |
//! | `random-module-generation.o` | the `%WasmGenerateRandomModule` test native (`--allow-natives-syntax`) | 204 KB |
//! | `liveedit.o` | `debug::Script::SetScriptSource` (inspector only, not linked) and the `%LiveEditPatchScript` test native | 37 KB |
//! | `debug-coverage.o` | the `debug::Coverage` API (inspector only) and the `%DebugCollectCoverage` / `%DebugToggle*Coverage` test natives | 34 KB |
//! | `verifier.o` | `--turbo-verify` (on only in debug builds of V8) | 22 KB |
//! | `simplified-lowering-verifier.o` | `--verify-simplified-lowering` | 17 KB |
//! | `register-allocator-verifier.o` | `--turbo-verify-allocation` (on only in debug builds of V8) | 17 KB |
//! | `graph-visualizer.o` | `--trace-turbo` (Turboshaft JSON graphs for Turbolizer) | 17 KB |
//! | `experimental-interpreter.o`, `experimental-compiler.o` | the linear-time regexp engine: `--enable-experimental-regexp-engine` (the `l` flag), `--default-to-experimental-regexp-engine`, `--enable-experimental-regexp-engine-on-excessive-backtracks` | 26 + 12 KB |

/// Flag names (with `_` as `-`) whose value would reach a stub here, or
/// that imply one that would. [`super::init_v8`] refuses them.
pub const REFUSED_FLAGS: &[&str] = &[
    "turbolev",
    "turbolev-future",
    "turboshaft-assert-types",
    "turboshaft-typed-optimizations",
    "turboshaft-loop-optimization",
    "turboshaft-random-rescheduling",
    "wasm-random-rescheduling",
    "turbo-profiling",
    "turbo-profiling-verbose",
    "wasm-revectorize",
    "experimental-wasm-revectorize",
    "experimental-fuzzing",
    "allow-natives-syntax",
    "allow-natives-for-differential-fuzzing",
    "turbo-verify",
    "turbo-verify-allocation",
    "verify-simplified-lowering",
    "trace-turbo",
    "enable-experimental-regexp-engine",
    "default-to-experimental-regexp-engine",
    "experimental-regexp-engine-capture-group-opt",
    "enable-experimental-regexp-engine-on-excessive-backtracks",
];

/// The flags in `flags` (a V8 flag string) that [`REFUSED_FLAGS`] lists,
/// unless negated (`--no-x`, `--nox`) or set to false (`--x=false`).
pub fn refused_flags(flags: &str) -> Vec<String> {
    flags
        .split_whitespace()
        .filter_map(|tok| {
            let body = tok.trim_start_matches('-').replace('_', "-");
            let (name, value) = match body.split_once('=') {
                Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
                None => (body, None),
            };
            if value.as_deref() == Some("false") || value.as_deref() == Some("0") {
                return None;
            }
            REFUSED_FLAGS
                .contains(&name.as_str())
                .then(|| tok.to_owned())
        })
        .collect()
}

#[cold]
fn unreachable_v8(path: &str) -> ! {
    eprintln!("lightbrowser: V8 reached {path}, which is not linked (lb-js unused_v8)");
    std::process::abort()
}

/// Defines `extern "C"` functions under the given mangled C++ names, and
/// [`STUBS`] listing them. They take no arguments and never return, so the
/// C++ signature does not matter: the caller's arguments are ignored and
/// no value is returned.
macro_rules! stubs {
    ($($path:literal => [$($sym:literal),+ $(,)?]),+ $(,)?) => {
        /// Each stubbed path and the archive symbols defined for it.
        pub const STUBS: &[(&str, &[&str])] = &[$(($path, &[$($sym),+])),+];
        $($(
            // SAFETY (`unsafe` attribute): the name is a C++ symbol of the
            // prebuilt V8 archive, defined here so the linker does not
            // extract its object (module docs). Nothing else in the binary
            // defines it: a second definition is a link error.
            const _: () = {
                #[unsafe(export_name = $sym)]
                extern "C" fn stub() -> ! {
                    unreachable_v8($path)
                }
            };
        )+)+
    };
}

stubs! {
    "Turbolev graph building (--turbolev)" => [
        "_ZN2v88internal8compiler10turboshaft13PrintBytecodeERNS2_12PipelineDataEPNS0_6maglev21MaglevCompilationInfoE",
        "_ZN2v88internal8compiler10turboshaft26TurbolevGraphBuildingPhase3RunEPNS2_12PipelineDataEPNS0_4ZoneEPNS0_6maglev5GraphE",
    ],
    "the Turbolev frontend (--turbolev)" => [
        "_ZN2v88internal8compiler10turboshaft24TurbolevFrontendPipeline13PrintBytecodeEv",
        "_ZN2v88internal8compiler10turboshaft24TurbolevFrontendPipeline16PrintMaglevGraphENS0_6maglev11MaglevPhaseE",
        "_ZN2v88internal8compiler10turboshaft24TurbolevFrontendPipeline26PrintInliningTreeDebugInfoEv",
        "_ZN2v88internal8compiler10turboshaft24TurbolevFrontendPipeline26TurbolevPhaseMatchesFilterENS0_6maglev11MaglevPhaseE",
        "_ZN2v88internal8compiler10turboshaft24TurbolevFrontendPipeline3RunEv",
        "_ZN2v88internal8compiler10turboshaft24TurbolevFrontendPipelineC1EPNS2_12PipelineDataEPNS1_7LinkageE",
        "_ZN2v88internal8compiler10turboshaft24TurbolevFrontendPipelineC2EPNS2_12PipelineDataEPNS1_7LinkageE",
    ],
    "Wasm-in-JS inlining (Turbolev only)" => [
        "_ZN2v88internal8compiler10turboshaft21WasmInJSInliningPhase3RunEPNS2_12PipelineDataEPNS0_4ZoneE",
    ],
    "TypeAssertionsPhase (--turboshaft-assert-types)" => [
        "_ZN2v88internal8compiler10turboshaft19TypeAssertionsPhase3RunEPNS2_12PipelineDataEPNS0_4ZoneE",
    ],
    "TypedOptimizationsPhase (--turboshaft-typed-optimizations)" => [
        "_ZN2v88internal8compiler10turboshaft23TypedOptimizationsPhase3RunEPNS2_12PipelineDataEPNS0_4ZoneE",
    ],
    "LoopOptimizationPhase (--turboshaft-loop-optimization)" => [
        "_ZN2v88internal8compiler10turboshaft21LoopOptimizationPhase3RunEPNS2_12PipelineDataEPNS0_4ZoneE",
    ],
    "RandomReschedulingPhase (--turboshaft-random-rescheduling)" => [
        "_ZN2v88internal8compiler10turboshaft23RandomReschedulingPhase3RunEPNS2_12PipelineDataEPNS0_4ZoneE",
    ],
    "BlockInstrumentationPhase (--turbo-profiling)" => [
        "_ZN2v88internal8compiler10turboshaft25BlockInstrumentationPhase3RunEPNS2_12PipelineDataEPNS0_4ZoneE",
    ],
    "WasmRevecPhase (--wasm-revectorize)" => [
        "_ZN2v88internal8compiler10turboshaft14WasmRevecPhase3RunEPNS2_12PipelineDataEPNS0_4ZoneE",
    ],
    "Wasm random module generation (%WasmGenerateRandomModule)" => [
        "_ZN2v88internal4wasm7fuzzing24GenerateRandomWasmModuleEPNS0_4ZoneENS2_27WasmModuleGenerationOptionsENS_4base6VectorIKhEEPNSt4__Cr6vectorINS2_10ExportDataENSA_9allocatorISC_EEEESG_",
        "_ZN2v88internal4wasm7fuzzing26GenerateWasmModuleForDeoptEPNS0_4ZoneENS_4base6VectorIKhEERNSt4__Cr6vectorINS9_12basic_stringIcNS9_11char_traitsIcEENS9_9allocatorIcEEEENSE_ISG_EEEESJ_",
        "_ZN2v88internal4wasm7fuzzing26GenerateWasmModuleForRevecEPNS0_4ZoneENS_4base6VectorIKhEE",
        "_ZN2v88internal4wasm7fuzzing36GenerateWasmModuleForInitExpressionsEPNS0_4ZoneENS_4base6VectorIKhEEPm",
    ],
    "LiveEdit (debugger script patching; %LiveEditPatchScript)" => [
        "_ZN2v88internal8LiveEdit11PatchScriptEPNS0_7IsolateENS0_6HandleINS0_6ScriptEEENS4_INS0_6StringEEEbbPNS_5debug14LiveEditResultE",
        "_ZN2v88internal8LiveEdit14CompareStringsEPNS0_7IsolateENS0_6HandleINS0_6StringEEES6_PNSt4__Cr6vectorINS0_17SourceChangeRangeENS7_9allocatorIS9_EEEE",
        "_ZN2v88internal8LiveEdit17TranslatePositionERKNSt4__Cr6vectorINS0_17SourceChangeRangeENS2_9allocatorIS4_EEEEi",
    ],
    "code coverage (debug API, %DebugCollectCoverage)" => [
        "_ZN2v88internal8Coverage10SelectModeEPNS0_7IsolateENS_5debug12CoverageModeE",
        "_ZN2v88internal8Coverage14CollectPreciseEPNS0_7IsolateE",
        "_ZN2v88internal8Coverage15CollectWasmDataEPNS0_7IsolateE",
        "_ZN2v88internal8Coverage17CollectBestEffortEPNS0_7IsolateE",
        "_ZN2v88internal8Coverage7CollectEPNS0_7IsolateENS_5debug12CoverageModeE",
    ],
    "the Turbofan graph verifier (--turbo-verify)" => [
        "_ZN2v88internal8compiler16ScheduleVerifier3RunEPNS1_8ScheduleE",
        "_ZN2v88internal8compiler8Verifier3RunEPNS1_7TFGraphENS2_6TypingENS2_11CheckInputsENS2_8CodeTypeE",
        "_ZN2v88internal8compiler8Verifier7Visitor11CheckSwitchEPNS1_4NodeERKNS1_8AllNodesE",
        "_ZN2v88internal8compiler8Verifier7Visitor5CheckEPNS1_4NodeERKNS1_8AllNodesE",
    ],
    "the simplified-lowering verifier (--verify-simplified-lowering)" => [
        "_ZN2v88internal8compiler18IsModuloTruncationERKNS1_10TruncationE",
        "_ZN2v88internal8compiler26SimplifiedLoweringVerifier11CheckAndSetEPNS1_4NodeERKNS1_4TypeERKNS1_10TruncationE",
        "_ZN2v88internal8compiler26SimplifiedLoweringVerifier14JoinTruncationERKNS1_10TruncationES5_",
        "_ZN2v88internal8compiler26SimplifiedLoweringVerifier28ReportInvalidTypeCombinationEPNS1_4NodeERKNSt4__Cr6vectorINS1_4TypeENS5_9allocatorIS7_EEEE",
        "_ZN2v88internal8compiler26SimplifiedLoweringVerifier9CheckTypeEPNS1_4NodeERKNS1_4TypeE",
        "_ZN2v88internal8compiler26SimplifiedLoweringVerifier9VisitNodeEPNS1_4NodeERNS1_14OperationTyperE",
        "_ZN2v88internal8compiler29IsNonTruncatingMachineTypeForERKNS0_11MachineTypeERKNS1_4TypeEPNS0_4ZoneE",
        "_ZNK2v88internal8compiler26SimplifiedLoweringVerifier20GeneralizeTruncationERKNS1_10TruncationERKNS1_4TypeE",
    ],
    "the register allocator verifier (--turbo-verify-allocation)" => [
        "_ZN2v88internal8compiler16BlockAssessments12PerformMovesEPKNS1_11InstructionE",
        "_ZN2v88internal8compiler16BlockAssessments13DropRegistersEv",
        "_ZN2v88internal8compiler16BlockAssessments17CheckReferenceMapEPKNS1_12ReferenceMapE",
        "_ZN2v88internal8compiler16BlockAssessments20PerformParallelMovesEPKNS1_12ParallelMoveE",
        "_ZN2v88internal8compiler16BlockAssessments25IsStaleReferenceStackSlotENS1_18InstructionOperandENSt4__Cr8optionalIiEE",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier10VerifyTempERKNS2_17OperandConstraintE",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier11ValidateUseENS1_9RpoNumberEPNS1_16BlockAssessmentsENS1_18InstructionOperandEi",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier11VerifyInputERKNS2_17OperandConstraintE",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier12VerifyOutputERKNS2_17OperandConstraintE",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier14CreateForBlockEPKNS1_16InstructionBlockE",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier14VerifyGapMovesEv",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier15BuildConstraintEPKNS1_18InstructionOperandEPNS2_17OperandConstraintE",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier15CheckConstraintEPKNS1_18InstructionOperandEPKNS2_17OperandConstraintE",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier16VerifyAssignmentEPKc",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifier25ValidatePendingAssessmentENS1_9RpoNumberENS1_18InstructionOperandEPKNS1_16BlockAssessmentsEPNS1_17PendingAssessmentEi",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifierC1EPNS0_4ZoneEPKNS0_21RegisterConfigurationEPKNS1_19InstructionSequenceEPKNS1_5FrameE",
        "_ZN2v88internal8compiler25RegisterAllocatorVerifierC2EPNS0_4ZoneEPKNS0_21RegisterConfigurationEPKNS1_19InstructionSequenceEPKNS1_5FrameE",
        "_ZNK2v88internal8compiler16BlockAssessments5PrintEv",
    ],
    "Turboshaft JSON graph tracing (--trace-turbo)" => [
        "_ZN2v88internal8compiler10turboshaft25JSONTurboshaftGraphWriter10PrintEdgesEv",
        "_ZN2v88internal8compiler10turboshaft25JSONTurboshaftGraphWriter10PrintNodesEv",
        "_ZN2v88internal8compiler10turboshaft25JSONTurboshaftGraphWriter11PrintBlocksEv",
        "_ZN2v88internal8compiler10turboshaft25JSONTurboshaftGraphWriter5PrintEv",
        "_ZN2v88internal8compiler10turboshaft25JSONTurboshaftGraphWriterC1ERNSt4__Cr13basic_ostreamIcNS4_11char_traitsIcEEEERKNS2_5GraphEPNS1_15NodeOriginTableEPNS0_4ZoneE",
        "_ZN2v88internal8compiler10turboshaft25JSONTurboshaftGraphWriterC2ERNSt4__Cr13basic_ostreamIcNS4_11char_traitsIcEEEERKNS2_5GraphEPNS1_15NodeOriginTableEPNS0_4ZoneE",
        "_ZN2v88internal8compiler10turboshaft33PrintTurboshaftCustomDataPerBlockERNSt4__Cr14basic_ofstreamIcNS3_11char_traitsIcEEEEPKcRKNS2_5GraphENS3_8functionIFbRNS3_13basic_ostreamIcS6_EESD_NS2_10BlockIndexEEEE",
        "_ZN2v88internal8compiler10turboshaft37PrintTurboshaftCustomDataPerOperationERNSt4__Cr14basic_ofstreamIcNS3_11char_traitsIcEEEEPKcRKNS2_5GraphENS3_8functionIFbRNS3_13basic_ostreamIcS6_EESD_NS2_7OpIndexEEEE",
        "_ZN2v88internal8compiler10turboshaftlsERNSt4__Cr13basic_ostreamIcNS3_11char_traitsIcEEEERKNS2_21TurboshaftGraphAsJSONE",
    ],
    "the experimental regexp interpreter (--enable-experimental-regexp-engine*)" => [
        "_ZN2v88internal6regexp29ExperimentalRegExpInterpreter11FindMatchesEPNS0_7IsolateENS0_6RegExp10CallOriginENS0_6TaggedINS0_16TrustedByteArrayEEEiNS7_INS0_6StringEEEiPiiPNS0_4ZoneE",
    ],
    "the experimental regexp compiler (--enable-experimental-regexp-engine*)" => [
        "_ZN2v88internal6regexp20ExperimentalCompiler12CanBeHandledEPNS1_4TreeENS_4base5FlagsINS1_4FlagEiiEEi",
        "_ZN2v88internal6regexp20ExperimentalCompiler7CompileEPNS1_4TreeENS_4base5FlagsINS1_4FlagEiiEEPNS0_4ZoneE",
    ],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_flags_ignore_negations() {
        assert_eq!(
            refused_flags(
                "--jitless --turbolev --no-trace-turbo --trace_turbo=false \
                 --allow_natives_syntax --noturbo-verify --wasm-revectorize=true"
            ),
            [
                "--turbolev",
                "--allow_natives_syntax",
                "--wasm-revectorize=true"
            ]
        );
        assert!(refused_flags("").is_empty());
    }
}
