use super::schedule_ops::{
    SessionRoute, parse_delegate_arg, parse_jail_arg, session_route, split_schedule_spec,
};
use super::security_ops::{TrustRoute, parse_trust_domain, trust_route};

#[cfg(test)]
mod schedule_spec_tests {
    use super::split_schedule_spec;

    #[test]
    fn splits_cron_spec() {
        let (spec, prompt) = split_schedule_arg("*/5 * * * * run the tests");
        assert_eq!(spec, "*/5 * * * *");
        assert_eq!(prompt, "run the tests");
    }

    #[test]
    fn splits_compact_countdown() {
        let (spec, prompt) = split_schedule_arg("10m re-run the tests");
        assert_eq!(spec, "10m");
        assert_eq!(prompt, "re-run the tests");
    }

    #[test]
    fn splits_verbose_countdown() {
        let (spec, prompt) = split_schedule_arg("in 2 hours 30 minutes do the thing");
        assert_eq!(spec, "in 2 hours 30 minutes");
        assert_eq!(prompt, "do the thing");
    }

    #[test]
    fn splits_absolute_clock() {
        let (spec, prompt) = split_schedule_arg("14:00 ship the build");
        assert_eq!(spec, "14:00");
        assert_eq!(prompt, "ship the build");
    }

    #[test]
    fn splits_tomorrow_phrase() {
        let (spec, prompt) = split_schedule_arg("tomorrow 09:00 morning standup");
        assert_eq!(spec, "tomorrow 09:00");
        assert_eq!(prompt, "morning standup");
    }

    fn split_schedule_arg(rest: &str) -> (String, String) {
        split_schedule_spec(rest).expect("schedule spec must parse")
    }

    #[test]
    fn missing_prompt_is_none_not_empty_strings() {
        assert!(split_schedule_spec("10m").is_none());
        assert!(split_schedule_spec("14:00").is_none());
        assert!(split_schedule_spec("").is_none());
    }
}

#[cfg(test)]
mod session_route_tests {
    use super::{SessionRoute, session_route};

    fn parts(cmd: &str) -> Vec<&str> {
        cmd.split_whitespace().collect()
    }

    #[test]
    fn canonical_sessions_forms() {
        assert_eq!(
            session_route("sessions", &parts("/sessions")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("sessions", &parts("/sessions abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
    }

    #[test]
    fn legacy_resume_keeps_its_id_slot() {
        assert_eq!(
            session_route("resume", &parts("/resume")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("resume", &parts("/resume abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
    }

    #[test]
    fn legacy_session_subcommands_translate() {
        assert_eq!(
            session_route("session", &parts("/session open abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
        assert_eq!(
            session_route("session", &parts("/session resume abc123")),
            Ok(SessionRoute::Open(Some("abc123")))
        );
        assert_eq!(
            session_route("session", &parts("/session")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("session", &parts("/session open")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("session", &parts("/session list")),
            Ok(SessionRoute::Open(None))
        );
        assert_eq!(
            session_route("session", &parts("/session new")),
            Ok(SessionRoute::New)
        );
        assert_eq!(
            session_route("session", &parts("/session fork")),
            Ok(SessionRoute::Fork)
        );
        assert_eq!(
            session_route("session", &parts("/session status")),
            Ok(SessionRoute::Status)
        );
    }

    #[test]
    fn unknown_legacy_subcommand_is_an_error() {
        let err = session_route("session", &parts("/session frobnicate")).unwrap_err();
        assert!(
            err.contains("/session is retired"),
            "error should steer away from the retired command: {err}"
        );
    }
}

#[cfg(test)]
mod trust_route_tests {
    use super::{TrustRoute, trust_route};
    use muta_contracts::TrustDomain;

    fn parts(command: &str) -> Vec<&str> {
        command.split_whitespace().collect()
    }

    #[test]
    fn canonical_trust_grammar_is_closed() {
        assert_eq!(
            trust_route("trust", &parts("/trust")),
            Ok(TrustRoute::GrantAll)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust all")),
            Ok(TrustRoute::GrantAll)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust mcp")),
            Ok(TrustRoute::Grant(TrustDomain::Mcp))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust skills")),
            Ok(TrustRoute::Grant(TrustDomain::Skills))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust hooks")),
            Ok(TrustRoute::Grant(TrustDomain::Hooks))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust instructions")),
            Ok(TrustRoute::Grant(TrustDomain::Instructions))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust agents")),
            Ok(TrustRoute::Grant(TrustDomain::Instructions))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust rules")),
            Ok(TrustRoute::Grant(TrustDomain::Instructions))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust ex-workspace")),
            Ok(TrustRoute::Grant(TrustDomain::ExWorkspace))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust externals")),
            Ok(TrustRoute::Grant(TrustDomain::ExWorkspace))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust roots")),
            Ok(TrustRoute::Grant(TrustDomain::ExWorkspace))
        );
        assert_eq!(
            trust_route("trust", &parts("/trust status")),
            Ok(TrustRoute::Status)
        );
        assert_eq!(
            trust_route("trust", &parts("/trust revoke")),
            Ok(TrustRoute::Revoke)
        );
    }

    #[test]
    fn untrust_takes_no_arguments() {
        assert_eq!(
            trust_route("untrust", &parts("/untrust")),
            Ok(TrustRoute::Revoke)
        );
        assert!(trust_route("untrust", &parts("/untrust all")).is_err());
        assert!(trust_route("untrust", &parts("/untrust mcp")).is_err());
    }

    #[test]
    fn unknown_trust_subcommand_is_an_error() {
        let err = trust_route("trust", &parts("/trust frobnicate")).unwrap_err();
        assert!(
            err.contains("Unknown /trust subcommand 'frobnicate'"),
            "unexpected error message: {err}"
        );
    }
}

#[cfg(test)]
mod delegate_arg_tests {
    use super::parse_delegate_arg;

    #[test]
    fn empty_arg_is_toggle() {
        assert_eq!(parse_delegate_arg(""), Ok(None));
    }

    #[test]
    fn truthy_forms() {
        for s in ["on", "true", "1", "delegate", "auto", "yolo"] {
            assert_eq!(parse_delegate_arg(s), Ok(Some(true)), "failed on {s:?}");
        }
    }

    #[test]
    fn falsy_forms() {
        for s in ["off", "false", "0"] {
            assert_eq!(parse_delegate_arg(s), Ok(Some(false)), "failed on {s:?}");
        }
    }

    #[test]
    fn unknown_forms_error() {
        assert!(parse_delegate_arg("yes").is_err());
        assert!(parse_delegate_arg("no").is_err());
        assert!(parse_delegate_arg("random").is_err());
    }
}

#[cfg(test)]
mod jail_arg_tests {
    use super::parse_jail_arg;

    #[test]
    fn empty_arg_is_toggle() {
        assert_eq!(parse_jail_arg(""), Ok(None));
        assert_eq!(parse_jail_arg("   "), Ok(None));
    }

    #[test]
    fn enable_forms() {
        for s in ["on", "true", "1", "enable", "enabled", "confined", "jail"] {
            assert_eq!(parse_jail_arg(s), Ok(Some(true)), "failed on {s:?}");
        }
    }

    #[test]
    fn disable_forms() {
        for s in [
            "off",
            "false",
            "0",
            "disable",
            "disabled",
            "unconfined",
            "escape",
        ] {
            assert_eq!(parse_jail_arg(s), Ok(Some(false)), "failed on {s:?}");
        }
    }

    #[test]
    fn unknown_forms_error() {
        assert!(parse_jail_arg("yes").is_err());
        assert!(parse_jail_arg("no").is_err());
        assert!(parse_jail_arg("sandbox").is_err());
    }
}

#[cfg(test)]
mod trust_domain_tests {
    use super::parse_trust_domain;
    use muta_contracts::TrustDomain;

    #[test]
    fn known_domains_parse() {
        assert_eq!(parse_trust_domain("mcp"), Ok(TrustDomain::Mcp));
        assert_eq!(parse_trust_domain("skills"), Ok(TrustDomain::Skills));
        assert_eq!(parse_trust_domain("hooks"), Ok(TrustDomain::Hooks));
        assert_eq!(
            parse_trust_domain("instructions"),
            Ok(TrustDomain::Instructions)
        );
        assert_eq!(parse_trust_domain("agents"), Ok(TrustDomain::Instructions));
        assert_eq!(parse_trust_domain("rules"), Ok(TrustDomain::Instructions));
        assert_eq!(
            parse_trust_domain("ex-workspace"),
            Ok(TrustDomain::ExWorkspace)
        );
        assert_eq!(
            parse_trust_domain("externals"),
            Ok(TrustDomain::ExWorkspace)
        );
        assert_eq!(parse_trust_domain("roots"), Ok(TrustDomain::ExWorkspace));
    }

    #[test]
    fn unknown_domain_is_an_error() {
        let err = parse_trust_domain("unknown").unwrap_err();
        assert!(
            err.contains("Unknown trust domain `unknown`"),
            "unexpected error message: {err}"
        );
    }

    #[tokio::test]
    async fn reload_trusted_assets_updates_roots_on_trust() {
        use crate::handlers_slash::security_ops;
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        let external = tmp.path().join("extra");
        std::fs::create_dir_all(root.join(".muta")).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(
            root.join(".muta/config.toml"),
            "[workspace]\nadditional_roots = [\"../extra\"]\n",
        )
        .unwrap();

        let sec_file = tmp.path().join("state/security.json");
        let store =
            muta_persistence::workspace_security::WorkspaceSecurityStore::load_from(sec_file);
        let agent = Arc::new(muta_agent::Agent::new(
            Arc::new(muta_agent::NoProvider),
            vec![],
            muta_contracts::AgentIdentity::default(),
        ));
        let mcp = Arc::new(muta_mcp::McpRuntime::start_background(
            Default::default(),
            agent.dynamic_tool_sink(),
        ));
        let skills = muta_skills::SkillRegistry::empty();
        let shared_roots = muta_contracts::SharedAdditionalRoots::empty();

        // Initially untrusted: roots should remain quarantined and empty
        let report = security_ops::reload_trusted_assets(
            &agent,
            &mcp,
            &store,
            &root,
            &skills,
            &shared_roots,
        )
        .await
        .unwrap();
        assert_eq!(
            report.snapshot.ex_workspace,
            muta_contracts::WorkspaceTrustState::Quarantined
        );
        assert!(shared_roots.snapshot().is_empty());

        // Trust ex-workspace domain: additional roots are dynamically resolved and stored in shared_roots
        store.trust_domain(&root, TrustDomain::ExWorkspace).unwrap();
        let report = security_ops::reload_trusted_assets(
            &agent,
            &mcp,
            &store,
            &root,
            &skills,
            &shared_roots,
        )
        .await
        .unwrap();
        assert_eq!(
            report.snapshot.ex_workspace,
            muta_contracts::WorkspaceTrustState::Trusted
        );
        let canonical_extra = std::fs::canonicalize(&external).unwrap();
        assert_eq!(shared_roots.snapshot(), vec![canonical_extra]);

        // Revoke trust: roots quarantined and shared_roots cleared again
        store.revoke_workspace(&root).unwrap();
        let report = security_ops::reload_trusted_assets(
            &agent,
            &mcp,
            &store,
            &root,
            &skills,
            &shared_roots,
        )
        .await
        .unwrap();
        assert_eq!(
            report.snapshot.ex_workspace,
            muta_contracts::WorkspaceTrustState::Quarantined
        );
        assert!(shared_roots.snapshot().is_empty());
    }
}
