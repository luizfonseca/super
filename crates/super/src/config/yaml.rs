//! YAML configurations, read with `serde-saphyr`: figment's own YAML format reads them with
//! `serde_yaml`, which is deprecated (and its `unsafe-libyaml` unmaintained, RUSTSEC-2024-0449)

/// The YAML format, for figment: `Yaml::file`, `Yaml::from_str`... (`figment::providers::Format`)
pub struct Yaml;

impl figment::providers::Format for Yaml {
    type Error = serde_saphyr::Error;

    const NAME: &'static str = "YAML";

    fn from_str<T: serde::de::DeserializeOwned>(string: &str) -> Result<T, Self::Error> {
        serde_saphyr::from_str(string)
    }
}
