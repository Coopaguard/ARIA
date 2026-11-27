//! The hooks, as a delegated session manages them. See [`crate::hooks`].
//!
//! # Why on the sessions' post and not the voice post
//!
//! The voice post's catalogue is held under 3150 bytes by a test, with 133 to
//! spare on 2026-11-27: a tool does not fit. And it does not need to — « liste
//! mes hooks », « coupe celui des mails » are delegated like any other task,
//! and creating a hook already is.
//!
//! # Which are answered without asking
//!
//! Two, decided with the user on 2026-11-27 after seeing the card: `hooks_list`
//! reads names, states and the *names* of secrets, never a value; and
//! `ask_secret`, whose card said nothing the form does not say better, and
//! whose form is itself the user's yes or no. See [`is_unasked`].
//!
//! `hook_set_enabled` goes through the relay like any tool. **Trying and
//! deleting do not rely on it**: in `auto` mode the CLI approves calls itself
//! and the relay is never asked, which is how a session validated its own hook
//! on 2026-11-27. Both ask the user from inside, through
//! [`crate::Core::confirm`]. A trial is the validation (2026-11-28): one run,
//! the user's verdict, the next run, the user's verdict; the first card
//! carries the script. There is no separate tool to validate.

/// The hooks' tools answered without a permission card.
pub fn is_unasked(naming: crate::providers::McpToolNaming, tool: &str) -> bool {
    ["hooks_list", "ask_secret"]
        .iter()
        .any(|name| naming.matches(tool, super::SERVER_NAME, name))
}

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{ErrorData, tool, tool_router};

use super::{Relay, deny};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct HookId {
    /// The hook's folder name under ~/.aria/hooks.
    pub id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SecretAsk {
    /// The hook, which must declare the secret in `secrets`.
    pub hook: String,
    /// The secret's name in `secrets`.
    pub name: String,
    /// What the user is to type, in French: « Mot de passe de la boîte mail ».
    pub label: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct HookSwitch {
    pub id: String,
    pub enabled: bool,
}

fn text(body: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(body.into())]))
}

#[tool_router(router = hooks_router, vis = "pub(crate)")]
impl Relay {
    #[tool(
        name = "hooks_list",
        description = "Liste les hooks d'ARIA (~/.aria/hooks) : état, rythme, derniers passages."
    )]
    pub async fn hooks_list(&self) -> Result<CallToolResult, ErrorData> {
        let Some(hooks) = self.core.hooks() else {
            return Ok(deny("Les hooks ne sont pas disponibles."));
        };
        let rows = self.core.hook_list();
        text(format!(
            "Dossier : {}\n{}",
            hooks.root().display(),
            serde_json::to_string(&rows).unwrap_or_default()
        ))
    }

    #[tool(
        name = "hook_trial",
        description = "Essaie un hook devant l'utilisateur et le fait valider : un passage, il juge \
             le résultat ; s'il convient, un second passage, il juge encore. Deux résultats \
             conformes valident le hook, qui tourne alors seul. Attend ses réponses."
    )]
    pub async fn hook_trial(
        &self,
        Parameters(params): Parameters<HookId>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(session) = self.session_of(&context) else {
            return Ok(deny("ARIA n'a pas pu rattacher cet essai à une session."));
        };
        // The user's cards, whatever the CLI's mode: see `Core::confirm`.
        match self.core.guided_trial(session, &params.id).await {
            Ok(said) => text(said),
            Err(e) => Ok(deny(&e.to_string())),
        }
    }

    #[tool(name = "hook_set_enabled", description = "Active ou coupe un hook déjà validé.")]
    pub async fn hook_set_enabled(
        &self,
        Parameters(params): Parameters<HookSwitch>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.core.hook_set_enabled(&params.id, params.enabled) {
            Ok(()) => text(if params.enabled { "Activé." } else { "Coupé." }),
            Err(e) => Ok(deny(&e.to_string())),
        }
    }

    #[tool(
        name = "ask_secret",
        description = "Demande à l'utilisateur un secret déclaré par un hook (mot de passe, jeton). \
             ARIA affiche son propre formulaire : tu ne vois jamais la valeur, le script la reçoit \
             en ARIA_SECRET_<NOM>. Attend sa réponse."
    )]
    pub async fn ask_secret(
        &self,
        Parameters(params): Parameters<SecretAsk>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.core.ask_secret(&params.hook, &params.name, &params.label).await {
            Ok(true) => text(format!("Le secret « {} » est enregistré.", params.name)),
            Ok(false) => Ok(deny("L'utilisateur a annulé : rien n'est enregistré.")),
            Err(e) => Ok(deny(&e.to_string())),
        }
    }

    #[tool(
        name = "hook_delete",
        description = "Supprime un hook, son dossier et ses secrets, après accord de l'utilisateur."
    )]
    pub async fn hook_delete(
        &self,
        Parameters(params): Parameters<HookId>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(session) = self.session_of(&context) else {
            return Ok(deny("ARIA n'a pas pu rattacher cette suppression à une session."));
        };
        let name = self
            .core
            .hooks()
            .map_or_else(|| params.id.clone(), |h| h.name(&params.id));
        let decision = self
            .core
            .confirm(
                session,
                "hook_delete",
                params.id.clone(),
                format!("supprimer le hook « {name} », son dossier et ses secrets"),
                crate::permissions::Risk::High,
            )
            .await;
        if !matches!(
            decision,
            crate::permissions::Decision::Allow | crate::permissions::Decision::AllowAlways { .. }
        ) {
            return Ok(deny("L'utilisateur a refusé : le hook est gardé."));
        }
        match self.core.hook_delete(&params.id) {
            Ok(_) => text(format!("Le hook {} est supprimé.", params.id)),
            Err(e) => Ok(deny(&e.to_string())),
        }
    }
}
