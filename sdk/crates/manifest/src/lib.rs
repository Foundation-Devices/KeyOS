// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppManifest {
    pub app_name: String,
    pub app_id: String,
    pub icon: String,
    pub permissions: Vec<String>,
    pub target_api_version: String,
}

impl AppManifest {
    pub fn example() -> Self {
        Self {
            app_name: "Hello World".to_string(),
            app_id: "com.foundation.hello-world".to_string(),
            icon: "icon.png".to_string(),
            permissions: vec!["nfc".to_string()],
            target_api_version: "1".to_string(),
        }
    }

    pub fn to_toml_string(&self) -> String {
        let permissions = if self.permissions.is_empty() {
            "[]".to_string()
        } else {
            format!(
                "[{}]",
                self.permissions
                    .iter()
                    .map(|permission| format!("\"{permission}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        format!(
            "[app]\nname = \"{}\"\napp_id = \"{}\"\nicon = \"{}\"\npermissions = {}\ntarget_api_version = \"{}\"\n",
            self.app_name, self.app_id, self.icon, permissions, self.target_api_version
        )
    }
}

#[cfg(test)]
mod tests {
    use super::AppManifest;

    #[test]
    fn example_manifest_renders_to_toml() {
        let rendered = AppManifest::example().to_toml_string();
        assert!(rendered.contains("name = \"Hello World\""));
        assert!(rendered.contains("target_api_version = \"1\""));
    }
}
