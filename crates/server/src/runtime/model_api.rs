use devo_core::ModelCatalogEntry;
use devo_core::ModelCatalogParams;
use devo_core::ModelCatalogResult;
use devo_core::ModelSavedEntry;
use devo_core::ModelSavedParams;
use devo_core::ModelSavedResult;
use devo_core::ProviderConfigFile;
use devo_core::ProviderWireApi;
use devo_core::parse_config_str;

use crate::{ProtocolErrorCode, SuccessResponse};

use super::ServerRuntime;

impl ServerRuntime {
    pub(super) async fn handle_model_catalog(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        if let Err(error) = serde_json::from_value::<ModelCatalogParams>(params) {
            return self.error_response(
                request_id,
                ProtocolErrorCode::InvalidParams,
                format!("invalid model/catalog params: {error}"),
            );
        }

        let catalog = &self.deps.model_catalog;
        let models: Vec<ModelCatalogEntry> = catalog
            .list_visible()
            .into_iter()
            .map(ModelCatalogEntry::from)
            .collect();

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: ModelCatalogResult { models },
        })
        .expect("serialize model/catalog response")
    }

    pub(super) async fn handle_model_saved(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        if let Err(error) = serde_json::from_value::<ModelSavedParams>(params) {
            return self.error_response(
                request_id,
                ProtocolErrorCode::InvalidParams,
                format!("invalid model/saved params: {error}"),
            );
        }

        let config = if self.deps.config_file.exists() {
            match std::fs::read_to_string(&self.deps.config_file) {
                Ok(contents) => parse_config_str(&contents).unwrap_or_default(),
                Err(_) => ProviderConfigFile::default(),
            }
        } else {
            ProviderConfigFile::default()
        };

        let catalog = &self.deps.model_catalog;
        let mut models = Vec::new();

        for (provider_id, provider_config) in &config.model_providers {
            let wire_api = provider_config
                .wire_api
                .unwrap_or(ProviderWireApi::OpenAIChatCompletions);
            for configured in &provider_config.models {
                let slug = configured.model.clone();
                let catalog_model = catalog.get(&slug);
                models.push(ModelSavedEntry {
                    slug: slug.clone(),
                    display_name: catalog_model
                        .map(|m| m.display_name.clone())
                        .unwrap_or_else(|| slug.clone()),
                    channel: catalog_model.and_then(|m| m.channel.clone()),
                    description: catalog_model.and_then(|m| m.description.clone()),
                    provider_id: provider_id.clone(),
                    wire_api,
                    context_window: catalog_model.map(|m| m.context_window).unwrap_or(200_000),
                });
            }
        }

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: ModelSavedResult { models },
        })
        .expect("serialize model/saved response")
    }
}
