#![no_main]
#![no_std]
#![feature(abi_efiapi)]
#![feature(negative_impls)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod linux_loader;
mod pe_loader;
mod pe_section;
mod uefi_helpers;

use pe_loader::Image;
use pe_section::{pe_section, pe_section_as_string};
use sha2::{Digest, Sha256};
use uefi::{
    prelude::*,
    proto::{
        console::text::Output,
        media::file::{File, FileAttribute, FileMode, RegularFile},
    },
    CString16, Result,
};

use crate::{
    linux_loader::InitrdLoader,
    uefi_helpers::{booted_image_file, read_all},
};

type Hash = sha2::digest::Output<Sha256>;

/// Print the startup logo on boot.
fn print_logo(output: &mut Output) -> Result<()> {
    output.clear()?;

    output.output_string(cstr16!(
        "
  _                      _                 _\r
 | |                    | |               | |\r
 | | __ _ _ __  ______ _| |__   ___   ___ | |_ ___\r
 | |/ _` | '_ \\|_  / _` | '_ \\ / _ \\ / _ \\| __/ _ \\\r
 | | (_| | | | |/ / (_| | |_) | (_) | (_) | ||  __/\r
 |_|\\__,_|_| |_/___\\__,_|_.__/ \\___/ \\___/ \\__\\___|\r
\r
"
    ))
}

/// The configuration that is embedded at build time.
///
/// After lanzaboote is built, lanzatool needs to embed configuration
/// into the binary. This struct represents that information.
struct EmbeddedConfiguration {
    /// The filename of the kernel to be booted. This filename is
    /// relative to the root of the volume that contains the
    /// lanzaboote binary.
    kernel_filename: CString16,

    /// The cryptographic hash of the kernel.
    kernel_hash: Hash,

    /// The filename of the initrd to be passed to the kernel. See
    /// `kernel_filename` for how to interpret these filenames.
    initrd_filename: CString16,

    /// The cryptographic hash of the initrd. This hash is computed
    /// over the whole PE binary, not only the embedded initrd.
    initrd_hash: Hash,

    /// The kernel command-line.
    cmdline: CString16,
}

/// Extract a string, stored as UTF-8, from a PE section.
fn extract_string(file_data: &[u8], section: &str) -> Result<CString16> {
    let string = pe_section_as_string(file_data, section).ok_or(Status::INVALID_PARAMETER)?;

    Ok(CString16::try_from(string.as_str()).map_err(|_| Status::INVALID_PARAMETER)?)
}

/// Extract a Blake3 hash from a PE section.
fn extract_hash(file_data: &[u8], section: &str) -> Result<Hash> {
    let array: [u8; 32] = pe_section(file_data, section)
        .ok_or(Status::INVALID_PARAMETER)?
        .try_into()
        .map_err(|_| Status::INVALID_PARAMETER)?;

    Ok(array.into())
}

impl EmbeddedConfiguration {
    fn new(file: &mut RegularFile) -> Result<Self> {
        file.set_position(0)?;
        let file_data = read_all(file)?;

        Ok(Self {
            kernel_filename: extract_string(&file_data, ".kernelp")?,
            kernel_hash: extract_hash(&file_data, ".kernelh")?,

            initrd_filename: extract_string(&file_data, ".initrdp")?,
            initrd_hash: extract_hash(&file_data, ".initrdh")?,

            cmdline: extract_string(&file_data, ".cmdline")?,
        })
    }
}

#[entry]
fn main(handle: Handle, mut system_table: SystemTable<Boot>) -> Status {
    uefi_services::init(&mut system_table).unwrap();

    print_logo(system_table.stdout()).unwrap();

    let config: EmbeddedConfiguration =
        EmbeddedConfiguration::new(&mut booted_image_file(system_table.boot_services()).unwrap())
            .expect("Failed to extract configuration from binary. Did you run lanzatool?");

    let kernel_data;
    let initrd_data;

    {
        let mut file_system = system_table
            .boot_services()
            .get_image_file_system(handle)
            .expect("Failed to get file system handle");
        let mut root = file_system
            .open_volume()
            .expect("Failed to find ESP root directory");

        let mut kernel_file = root
            .open(
                &config.kernel_filename,
                FileMode::Read,
                FileAttribute::empty(),
            )
            .expect("Failed to open kernel file for reading")
            .into_regular_file()
            .expect("Kernel is not a regular file");

        kernel_data = read_all(&mut kernel_file).expect("Failed to read kernel file into memory");

        let mut initrd_file = root
            .open(
                &config.initrd_filename,
                FileMode::Read,
                FileAttribute::empty(),
            )
            .expect("Failed to open initrd for reading")
            .into_regular_file()
            .expect("Initrd is not a regular file");

        initrd_data = read_all(&mut initrd_file).expect("Failed to read kernel file into memory");
    }

    if Sha256::digest(&kernel_data) != config.kernel_hash {
        system_table
            .stdout()
            .output_string(cstr16!("Hash mismatch for kernel. Refusing to load!\r\n"))
            .unwrap();
        return Status::SECURITY_VIOLATION;
    }

    if Sha256::digest(&initrd_data) != config.initrd_hash {
        system_table
            .stdout()
            .output_string(cstr16!("Hash mismatch for initrd. Refusing to load!\r\n"))
            .unwrap();
        return Status::SECURITY_VIOLATION;
    }

    let kernel =
        Image::load(system_table.boot_services(), &kernel_data).expect("Failed to load the kernel");

    let mut initrd_loader = InitrdLoader::new(system_table.boot_services(), handle, initrd_data)
        .expect("Failed to load the initrd. It may not be there or it is not signed");

    let status = unsafe { kernel.start(handle, &system_table, &config.cmdline) };

    initrd_loader
        .uninstall(system_table.boot_services())
        .expect("Failed to uninstall the initrd protocols");
    status
}