use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::esp::{EspGenerationPaths, EspPaths};
use crate::generation::{Generation, GenerationLink};

pub struct LoaderEntries {
    systemd_boot_loader_config: PathBuf,
    esp_paths: EspPaths,
    ble_paths: EspPaths,
    generation_links: Vec<PathBuf>,
}

impl LoaderEntries {
    pub fn new(
        systemd_boot_loader_config: PathBuf,
        esp: PathBuf,
        boot_loader_entries: PathBuf,
        generation_links: Vec<PathBuf>,
    ) -> Self {
        let esp_paths = EspPaths::new(esp);
        let ble_paths = EspPaths::new(boot_loader_entries);

        Self {
            systemd_boot_loader_config,
            esp_paths,
            ble_paths,
            generation_links,
        }
    }

    pub fn create(&mut self) -> Result<()> {
        let mut links = self
            .generation_links
            .iter()
            .map(GenerationLink::from_path)
            .collect::<Result<Vec<GenerationLink>>>()?;

        // Sort the links by version.
        links.sort_by_key(|l| l.version);

        // Create parent directories
        fs::create_dir_all(&self.ble_paths.loader).ok();
        fs::create_dir_all(&self.ble_paths.efi).ok();
        fs::create_dir_all(&self.ble_paths.loader_entries).ok();

        // Copy in relevant systemd-boot config and link kernel/initrd dir.
        fs::copy(
            &self.systemd_boot_loader_config,
            &self.ble_paths.systemd_boot_loader_config,
        )
        .with_context(|| {
            format!(
                "Failed to copy systemd-boot loader.conf to {:?}",
                &self.ble_paths.systemd_boot_loader_config
            )
        })?;

        symlink(&self.esp_paths.nixos, &self.ble_paths.nixos).with_context(|| {
            format!(
                "Failed to link nixos artifacts to {:?}",
                &self.ble_paths.nixos
            )
        })?;

        for link in links {
            let generation_result = Generation::from_link(&link)
                .with_context(|| format!("Failed to build generation from link: {link:?}"));

            // Ignore failing to read a generation so that old malformed generations do not stop
            // lzbt from working.
            let generation = match generation_result {
                Ok(generation) => generation,
                Err(e) => {
                    log::debug!(
                        "Ignoring generation {} because it's malformed.",
                        link.version
                    );
                    log::debug!("{e:#}");
                    continue;
                }
            };

            self.write_generation_entry(&generation)
                .context("Failed to build generation artifacts.")?;

            for (name, bootspec) in &generation.spec.bootspec.specialisation {
                let specialised_generation = generation.specialise(name, bootspec)?;

                self.write_generation_entry(&specialised_generation)
                    .context("Failed to build generation artifacts for specialisation.")?;
            }
        }

        Ok(())
    }

    fn write_generation_entry(&mut self, generation: &Generation) -> Result<()> {
        let bootspec = &generation.spec.bootspec;

        let esp_gen_paths = EspGenerationPaths::new(&self.ble_paths, generation)?;

        let mut entry = File::create(
            self.ble_paths.loader_entries.join(
                esp_gen_paths
                    .lanzaboote_image
                    .with_extension("conf")
                    .strip_prefix(&self.ble_paths.linux)?,
            ),
        )?;

        writeln!(&mut entry, "title {}", bootspec.label)?;
        writeln!(
            &mut entry,
            "linux {}",
            esp_gen_paths
                .kernel
                .strip_prefix(&self.ble_paths.esp)?
                .display()
        )?;
        writeln!(
            &mut entry,
            "initrd {}",
            esp_gen_paths
                .initrd
                .strip_prefix(&self.ble_paths.esp)?
                .display()
        )?;
        writeln!(
            &mut entry,
            "options init={} {}",
            bootspec.init.display(),
            bootspec.kernel_params.join(" ")
        )?;

        Ok(())
    }
}
