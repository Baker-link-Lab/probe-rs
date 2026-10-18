pub mod artifact_cache;

pub mod cmd {
    #[path = "../bin/probe-rs/cmd/dap_server/mod.rs"]
    pub mod dap_server;

    pub mod run {
        use std::path::Path;

        #[derive(Debug, PartialEq)]
        pub struct Test {
            pub name: String,
            pub address: Option<u32>,
        }

        #[derive(Debug, PartialEq)]
        pub struct EmbeddedTestElfInfo {
            pub version: u32,
            pub tests: Vec<Test>,
        }

        impl EmbeddedTestElfInfo {
            pub fn from_elf(_path: &Path) -> anyhow::Result<Option<Self>> {
                Ok(None)
            }
        }
    }
}

mod format;

pub use format::{FormatKind, FormatOptions};

pub mod util {
    #[path = "../bin/probe-rs/util/cargo.rs"]
    pub mod cargo;
    #[path = "../bin/probe-rs/util/common_options.rs"]
    pub mod common_options;
    #[path = "../bin/probe-rs/util/rtt.rs"]
    pub mod rtt;

    use std::num::ParseIntError;

    pub fn parse_u32(input: &str) -> Result<u32, ParseIntError> {
        parse_int::parse(input)
    }

    pub fn parse_u64(input: &str) -> Result<u64, ParseIntError> {
        parse_int::parse(input)
    }

    pub mod flash {
        use std::path::Path;

        use probe_rs::{InstructionSet, Session, flashing::FileDownloadError};

        use crate::FormatOptions;

        pub fn build_loader(
            session: &mut Session,
            path: impl AsRef<Path>,
            format_options: FormatOptions,
            image_instruction_set: Option<InstructionSet>,
        ) -> Result<probe_rs::flashing::FlashLoader, FileDownloadError> {
            #[allow(unreachable_patterns)]
            let format = match format_options.to_format_kind(session.target()) {
                probe_rs::flashing::FormatKind::Bin => {
                    probe_rs::flashing::Format::Bin(probe_rs::flashing::BinOptions {
                        base_address: format_options.bin_options.base_address,
                        skip: format_options.bin_options.skip,
                    })
                }
                probe_rs::flashing::FormatKind::Hex => probe_rs::flashing::Format::Hex,
                probe_rs::flashing::FormatKind::Elf => {
                    probe_rs::flashing::Format::Elf(probe_rs::flashing::ElfOptions {
                        skip_sections: format_options.elf_options.skip_section,
                    })
                }
                probe_rs::flashing::FormatKind::Uf2 => probe_rs::flashing::Format::Uf2,
                probe_rs::flashing::FormatKind::Idf => {
                    probe_rs::flashing::Format::Idf(probe_rs::flashing::IdfOptions {
                        bootloader: format_options.idf_options.idf_bootloader.map(From::from),
                        partition_table: format_options
                            .idf_options
                            .idf_partition_table
                            .map(From::from),
                        target_app_partition: format_options.idf_options.idf_target_app_partition,
                        flash_frequency: format_options.idf_options.idf_flash_freq.map(From::from),
                        flash_mode: format_options.idf_options.idf_flash_mode.map(From::from),
                    })
                }
                _ => unreachable!("Target format should be resolved before build_loader"),
            };

            probe_rs::flashing::build_loader(session, path, format, image_instruction_set)
        }
    }
}
