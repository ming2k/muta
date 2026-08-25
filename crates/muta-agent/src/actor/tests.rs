#[cfg(test)]
mod tests {
    use super::super::*;
    use muta_contracts::{ActorMessage, ActorRole, ActorState, WorktreeMode};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn actor_mailbox_and_handle_lifecycle() {
        let (mut mailbox, sender) = ActorMailbox::new();
        let cancel_token = CancellationToken::new();
        let handle = ActorHandle::new(
            "subagent_test_1".to_string(),
            Some("principal_1".to_string()),
            ActorRole::Research,
            WorktreeMode::Inherit,
            sender,
            cancel_token.clone(),
            ActorState::Idle,
        );

        assert_eq!(handle.state(), ActorState::Idle);
        assert!(!handle.is_terminated());

        // Send task
        handle
            .send_task("Research tokio channel patterns".to_string(), vec![])
            .unwrap();

        let env = mailbox.recv().await.expect("receive envelope");
        assert_eq!(env.recipient, "subagent_test_1");
        assert_eq!(env.sender.as_deref(), Some("principal_1"));
        if let ActorMessage::Task { prompt, .. } = env.message {
            assert_eq!(prompt, "Research tokio channel patterns");
        } else {
            panic!("Expected Task message");
        }

        // Test Supervisor
        let supervisor = ActorSupervisor::new(None);
        supervisor.register(handle.clone());
        assert!(supervisor.get("subagent_test_1").is_some());
        assert_eq!(supervisor.list_active().len(), 1);

        // Cancel
        handle.cancel("User aborted".to_string());
        assert_eq!(handle.state(), ActorState::Cancelling);
        assert!(cancel_token.is_cancelled());

        let cancel_env = mailbox.recv().await.expect("receive cancel envelope");
        if let ActorMessage::Cancel { reason } = cancel_env.message {
            assert_eq!(reason, "User aborted");
        } else {
            panic!("Expected Cancel message");
        }

        // Terminate
        handle.set_state(ActorState::Terminated);
        assert!(handle.is_terminated());
        supervisor.remove("subagent_test_1");
        assert_eq!(supervisor.list_active().len(), 0);
    }
}
