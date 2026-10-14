//! Choices remembered between launches.

use stem_split_core::{
    domain::{ModelId, OutputChoice, OutputDir},
    separate::DevicePreference,
};

use crate::view::text::Locale;

const LOCALE: &str = "locale";
const MODEL: &str = "model";
const DEVICE: &str = "device";
const OUTPUT_FOLDER: &str = "output_folder";

/// What the application remembers: the language and how to split, but not
/// which file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) locale: Locale,
    pub(crate) model: ModelId,
    pub(crate) device: DevicePreference,
    pub(crate) output: OutputChoice,
}

/// The name a device preference is saved under. Unlike the label shown in
/// the interface, it must never change, whatever the language.
const fn device_key(device: DevicePreference) -> &'static str {
    match device {
        DevicePreference::Auto => "auto",
        DevicePreference::Gpu => "gpu",
        DevicePreference::Cpu => "cpu",
    }
}

impl Settings {
    /// What a first launch starts with: the language of the system and the
    /// defaults of everything else.
    pub(crate) fn initial() -> Self {
        Self {
            locale: Locale::of_system(),
            model: ModelId::default(),
            device: DevicePreference::default(),
            output: OutputChoice::default(),
        }
    }

    /// Reads what an earlier launch saved.
    ///
    /// Each value is checked like any other outside input. One that is
    /// missing, unknown to this version or no longer valid (a folder that
    /// has been deleted) falls back to its initial value on its own, without
    /// discarding the others.
    pub(crate) fn load(storage: &dyn eframe::Storage) -> Self {
        let initial = Self::initial();
        let read = |key| storage.get_string(key);
        Self {
            locale: read(LOCALE)
                .and_then(|code| Locale::from_code(&code))
                .unwrap_or(initial.locale),
            model: read(MODEL)
                .and_then(|name| {
                    ModelId::ALL
                        .iter()
                        .copied()
                        .find(|model| model.name() == name)
                })
                .unwrap_or(initial.model),
            device: read(DEVICE)
                .and_then(|key| {
                    DevicePreference::ALL
                        .iter()
                        .copied()
                        .find(|&device| device_key(device) == key)
                })
                .unwrap_or(initial.device),
            output: read(OUTPUT_FOLDER)
                .and_then(|path| OutputDir::new(path).ok())
                .map_or(initial.output, OutputChoice::Custom),
        }
    }

    pub(crate) fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string(LOCALE, self.locale.code().to_owned());
        storage.set_string(MODEL, self.model.name().to_owned());
        storage.set_string(DEVICE, device_key(self.device).to_owned());
        let folder = match &self.output {
            OutputChoice::BesideInput => None,
            // A path that is not valid Unicode cannot be stored as text; the
            // next launch then starts with the default.
            OutputChoice::Custom(directory) => directory.path().to_str(),
        };
        match folder {
            Some(path) => storage.set_string(OUTPUT_FOLDER, path.to_owned()),
            None => storage.remove_string(OUTPUT_FOLDER),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, error::Error};

    use eframe::Storage as _;

    use super::*;

    /// Settings storage that lives in memory.
    #[derive(Debug, Default)]
    struct MemoryStorage(HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }

        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }

        fn flush(&mut self) {}
    }

    fn storage_with(entries: &[(&str, &str)]) -> MemoryStorage {
        MemoryStorage(
            entries
                .iter()
                .map(|&(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        )
    }

    #[test]
    fn empty_storage_gives_the_initial_settings() {
        assert_eq!(
            Settings::load(&MemoryStorage::default()),
            Settings::initial()
        );
        assert_eq!(Settings::initial().model, ModelId::Htdemucs);
        assert_eq!(Settings::initial().device, DevicePreference::Auto);
        assert_eq!(Settings::initial().output, OutputChoice::BesideInput);
    }

    #[test]
    fn saved_settings_are_read_back() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let settings = Settings {
            locale: Locale::Ru,
            model: ModelId::Htdemucs6s,
            device: DevicePreference::Cpu,
            output: OutputChoice::Custom(OutputDir::new(directory.path())?),
        };
        let mut storage = MemoryStorage::default();

        settings.save(&mut storage);

        assert_eq!(Settings::load(&storage), settings);
        Ok(())
    }

    #[test]
    fn every_model_device_and_language_survives_a_round_trip() {
        for &model in ModelId::ALL {
            for &device in DevicePreference::ALL {
                for &locale in Locale::ALL {
                    let settings = Settings {
                        locale,
                        model,
                        device,
                        output: OutputChoice::BesideInput,
                    };
                    let mut storage = MemoryStorage::default();
                    settings.save(&mut storage);
                    assert_eq!(Settings::load(&storage), settings);
                }
            }
        }
    }

    #[test]
    fn choosing_the_folder_of_the_file_forgets_the_other_folder() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mut settings = Settings {
            output: OutputChoice::Custom(OutputDir::new(directory.path())?),
            ..Settings::initial()
        };
        let mut storage = MemoryStorage::default();
        settings.save(&mut storage);

        settings.output = OutputChoice::BesideInput;
        settings.save(&mut storage);

        assert_eq!(storage.get_string(OUTPUT_FOLDER), None);
        assert_eq!(Settings::load(&storage).output, OutputChoice::BesideInput);
        Ok(())
    }

    #[test]
    fn folder_that_no_longer_exists_falls_back_without_losing_the_rest()
    -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let gone = directory.path().join("deleted since");
        let storage = storage_with(&[
            (LOCALE, "ru"),
            (MODEL, "htdemucs_ft"),
            (DEVICE, "gpu"),
            (OUTPUT_FOLDER, gone.to_str().ok_or("path is not Unicode")?),
        ]);

        let settings = Settings::load(&storage);

        assert_eq!(settings.output, OutputChoice::BesideInput);
        assert_eq!(settings.locale, Locale::Ru);
        assert_eq!(settings.model, ModelId::HtdemucsFt);
        assert_eq!(settings.device, DevicePreference::Gpu);
        Ok(())
    }

    #[test]
    fn unknown_values_fall_back_one_by_one() {
        let storage = storage_with(&[
            (LOCALE, "tlh"),
            (MODEL, "a model of the future"),
            (DEVICE, "cpu"),
        ]);

        let settings = Settings::load(&storage);

        assert_eq!(settings.locale, Settings::initial().locale);
        assert_eq!(settings.model, ModelId::Htdemucs);
        assert_eq!(settings.device, DevicePreference::Cpu);
    }

    #[test]
    fn device_keys_are_distinct() {
        let keys: std::collections::HashSet<&str> = DevicePreference::ALL
            .iter()
            .map(|&device| device_key(device))
            .collect();
        assert_eq!(keys.len(), DevicePreference::ALL.len());
    }
}
