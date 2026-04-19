#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DmAdmission {
    Allowed,
    /// Username matched; the caller should bind the sender's numeric user ID
    /// on the next message. The value is not carried here — `decide_inbound_admission`
    /// uses `sender_user_id` from its own parameter for the actual bind, so
    /// the two are always the same identity rather than relying on the
    /// caller to pass it through correctly.
    AllowedFirstBinding,
    Denied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InboundAdmission {
    Accept,
    AcceptAndBind(i64),
    RejectedNonPrivate,
    RejectedMissingIdentity,
    Denied,
}

/// Apply the bot-wide inbound admission policy to a single message.
///
/// The chat-type check runs first so non-private chats can never trigger
/// admission binding for the wrong chat_id. When binding is required, the
/// caller's `from.id` (not `chat.id`) is the identity bound.
pub(crate) fn decide_inbound_admission<F>(
    chat_type: &str,
    sender_user_id: Option<i64>,
    sender_username: Option<&str>,
    check_admission: F,
) -> InboundAdmission
where
    F: FnOnce(Option<&str>) -> DmAdmission,
{
    if chat_type != "private" {
        return InboundAdmission::RejectedNonPrivate;
    }
    match check_admission(sender_username) {
        DmAdmission::Allowed => InboundAdmission::Accept,
        DmAdmission::AllowedFirstBinding => match sender_user_id {
            Some(id) => InboundAdmission::AcceptAndBind(id),
            None => InboundAdmission::RejectedMissingIdentity,
        },
        DmAdmission::Denied => InboundAdmission::Denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decide_inbound_admission_rejects_group_before_any_admission_side_effect() {
        // Regression: a group user whose username matches the allowed DM
        // username must NOT cause check_dm_admission to be called, otherwise
        // bind_dm_user_id would persist a group chat_id and lock out the
        // operator's real DM forever.
        let mut admission_called = false;
        let outcome =
            decide_inbound_admission("supergroup", Some(7777), Some("operator"), |_username| {
                admission_called = true;
                DmAdmission::AllowedFirstBinding
            });
        assert!(matches!(outcome, InboundAdmission::RejectedNonPrivate));
        assert!(
            !admission_called,
            "admission must short-circuit on non-private chats"
        );
    }

    #[test]
    fn test_decide_inbound_admission_binds_sender_user_id_in_private_dm() {
        // First-message private DM with a matching username must bind by
        // from.id (not chat.id). They are equal in DMs, but from.id is the
        // identity invariant; this test pins that expectation.
        let outcome =
            decide_inbound_admission("private", Some(424242), Some("operator"), |_username| {
                DmAdmission::AllowedFirstBinding
            });
        assert!(matches!(outcome, InboundAdmission::AcceptAndBind(424242)));
    }

    #[test]
    fn test_decide_inbound_admission_rejects_first_binding_without_identity() {
        let outcome = decide_inbound_admission("private", None, Some("operator"), |_username| {
            DmAdmission::AllowedFirstBinding
        });
        assert!(matches!(outcome, InboundAdmission::RejectedMissingIdentity));
    }

    #[test]
    fn test_decide_inbound_admission_passes_through_denied() {
        let outcome =
            decide_inbound_admission("private", Some(123), Some("intruder"), |_username| {
                DmAdmission::Denied
            });
        assert!(matches!(outcome, InboundAdmission::Denied));
    }

    #[test]
    fn test_decide_inbound_admission_passes_through_already_bound_allow() {
        let outcome =
            decide_inbound_admission("private", Some(424242), Some("operator"), |_username| {
                DmAdmission::Allowed
            });
        assert!(matches!(outcome, InboundAdmission::Accept));
    }
}
