#![no_std]
#![feature(used_with_arg)]

extern crate ax_driver as _;

#[macro_use]
extern crate log;
#[macro_use]
extern crate ax_plat;

mod apic;
mod boot;
mod bootinfo;
mod console;
mod drivers;
mod init;
mod mem;
mod power;
mod screen;
mod time;
mod topology;

#[cfg(feature = "smp")]
mod mp;

pub mod config {
    //! Platform configuration module.
    //!
    //! If the `AX_CONFIG_PATH` environment variable is set, it will load the configuration from the specified path.
    //! Otherwise, it will fall back to the `axconfig.toml` file in the current directory and generate the default configuration.
    //!
    //! If the `PACKAGE` field in the configuration does not match the package name, it will panic with an error message.
    ax_config_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "axconfig.toml");
    assert_str_eq!(
        PACKAGE,
        env!("CARGO_PKG_NAME"),
        "`PACKAGE` field in the configuration does not match the Package name. Please check your \
         configuration file."
    );
}

unsafe extern "C" fn rust_entry(magic: usize, mbi: usize) {
    if magic == self::boot::MULTIBOOT_BOOTLOADER_MAGIC {
        ax_plat::call_main(0, mbi);
    } else if magic == 0x36d76289 {
        // Multiboot info pointers are aligned; preserve the protocol across BSS clearing.
        ax_plat::call_main(0, mbi | 1);
    }
}

unsafe extern "C" fn rust_entry_secondary(_magic: usize) {
    #[cfg(feature = "smp")]
    if _magic == self::boot::MULTIBOOT_BOOTLOADER_MAGIC {
        ax_plat::call_secondary_main(topology::current_cpu_id());
    }
}
