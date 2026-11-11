const START: &str = "/v1/auth/passkey/register/start";
const FINISH: &str = "/v1/auth/passkey/register/finish";
const EMAIL: &str = "receipt@example.com";
const CODE: &str = "admitted-invite";
const IP: &str = "198.51.100.9";

fn setup() -> Harness {
    let mut h = harness();
    h.config.invites_required = true;
    seed_user(&h, "issuer", "issuer@example.com");
    let hash = auth::hash_invite_code(CODE);
    h.store.invites.borrow_mut().insert(
        hash.clone(),
        InviteRecord::new_issued(
            hash,
            Some("issuer".to_string()),
            auth::INVITE_KIND_ADMIN,
            None,
            NOW,
            NOW + 604_800,
        ),
    );
    h
}

fn slots(h: &Harness, key: &str, action: &str) -> usize {
    h.store
        .rate_limit_slots
        .borrow()
        .iter()
        .filter(|((scope, act, _), expiry)| scope == key && act == action && **expiry > NOW)
        .count()
}

fn email_slots(h: &Harness, email: &str) -> usize {
    slots(
        h,
        &email_rate_limit_key(&h.config, email),
        "auth.invite_redeem_email",
    )
}

fn fixture_body(challenge: &str, token: &str, code: Option<&str>) -> serde_json::Value {
    let cred_id = b"receipt-credential";
    let att = fx_none_attestation(cred_id, &fx_es256_cose(91));
    serde_json::json!({
        "challenge": challenge,
        "credential_id": b64url_encode(cred_id),
        "attestation_object": b64url_encode(&att),
        "client_data_json": b64url_encode(fx_client_json("webauthn.create", challenge).as_bytes()),
        "magic_token": token,
        "invite_code": code,
    })
}

fn start(h: &Harness, email: &str, code: Option<&str>) -> serde_json::Value {
    let response = h.call(
        "POST",
        START,
        serde_json::json!({"email": email, "invite_code": code}),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let value: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(
        value.as_object().unwrap().len(),
        3,
        "no public receipt field"
    );
    value
}

fn admitted(h: &Harness) -> (serde_json::Value, serde_json::Value) {
    let value = start(h, EMAIL, Some(CODE));
    let token = h.mailer.sent.borrow().last().unwrap().1.clone();
    let finish = fixture_body(value["challenge"].as_str().unwrap(), &token, Some(CODE));
    (value, finish)
}

fn burn_email(h: &Harness, email: &str) {
    let key = email_rate_limit_key(&h.config, email);
    while email_slots(h, email) < auth::INVITE_EMAIL_MAX_ATTEMPTS as usize {
        assert!(block_on(h.store.reserve_rate_limit(
            &key,
            "auth.invite_redeem_email",
            u64::from(auth::INVITE_EMAIL_MAX_ATTEMPTS),
            NOW,
            auth::INVITE_EMAIL_WINDOW_SECS,
        ))
        .unwrap());
    }
}

fn assert_unclaimed(h: &Harness, start: &serde_json::Value, finish: &serde_json::Value) {
    let id = start["challenge"].as_str().unwrap();
    let magic_hash = auth::hash_magic_token(finish["magic_token"].as_str().unwrap());
    assert!(!h.store.challenges.borrow()[id].challenge.consumed);
    assert!(!h.store.magic.borrow()[&magic_hash].used);
    assert!(!h.store.invites.borrow()[&auth::hash_invite_code(CODE)].used);
    assert!(h.store.credentials.borrow().is_empty());
    assert!(h.store.sessions.borrow().is_empty());
    assert!(audits_with_action(h, auth::INVITE_REDEEMED_AUDIT_ACTION).is_empty());
}

#[test]
fn corrected_fifth_start_finishes_without_a_sixth_email_debit() {
    let h = setup();
    let mut refusals = Vec::new();
    for index in 0..4 {
        let response = h.call(
            "POST",
            START,
            serde_json::json!({
                "email": " RECEIPT@EXAMPLE.COM ", "invite_code": format!("wrong-{index}"),
            }),
        );
        assert_eq!(response.status, 401);
        assert!(response.set_cookie.is_none());
        refusals.push(response.body);
        assert_eq!(h.store.users.borrow().len(), 1, "only the issuer exists");
        assert!(h.store.challenges.borrow().is_empty());
        assert!(h.mailer.sent.borrow().is_empty());
        assert!(!h.store.invites.borrow()[&auth::hash_invite_code(CODE)].used);
    }
    assert!(refusals.iter().all(|body| body == &refusals[0]));
    let value = start(&h, " RECEIPT@EXAMPLE.COM ", Some(CODE));
    let id = value["challenge"].as_str().unwrap();
    let user = h.store.challenges.borrow()[id].user_id.clone().unwrap();
    assert_eq!(email_slots(&h, EMAIL), 5);
    let start_key = format!("invite:{}:start", rate_limit_ip_key(IP));
    assert_eq!(slots(&h, &start_key, "auth.invite_redeem"), 5);
    assert_eq!(h.store.challenges.borrow().len(), 1);
    assert_eq!(h.mailer.sent.borrow().len(), 1);
    let expected = InviteBudgetReceipt {
        invite_code_hash: auth::hash_invite_code(CODE),
        email_budget_key: email_rate_limit_key(&h.config, EMAIL),
    };
    assert_eq!(
        h.store.challenges.borrow()[id].invite_budget_receipt,
        Some(expected.clone())
    );
    assert!(!h.store.invites.borrow()[&expected.invite_code_hash].used);
    let token = h.mailer.sent.borrow()[0].1.clone();
    let finish = fixture_body(id, &token, Some(CODE));
    let response = h.call("POST", FINISH, finish.clone());
    assert_eq!(response.status, 200, "{}", response.body);
    assert!(response.set_cookie.is_some());
    assert_eq!(email_slots(&h, EMAIL), 5, "no reset and no sixth debit");
    assert_eq!(
        *h.store.invite_reads.borrow(),
        6,
        "always re-read the invite at finish"
    );
    assert_eq!(
        slots(
            &h,
            &format!("invite:{}:{user}", rate_limit_ip_key(IP)),
            "auth.invite_redeem"
        ),
        1
    );
    assert!(h.store.challenges.borrow()[id].challenge.consumed);
    assert_eq!(
        h.store.challenges.borrow()[id].invite_budget_receipt,
        Some(expected)
    );
    assert!(h.store.magic.borrow()[&auth::hash_magic_token(&token)].used);
    assert!(h.store.invites.borrow()[&auth::hash_invite_code(CODE)].used);
    assert_eq!(h.store.credentials.borrow().len(), 1);
    assert_eq!(h.store.sessions.borrow().len(), 1);
    assert_eq!(
        h.store
            .credentials
            .borrow()
            .values()
            .next()
            .unwrap()
            .user_id,
        user
    );
    assert_eq!(
        h.store.invited_by.borrow()[user.as_str()],
        Some("issuer".to_string())
    );
    assert_eq!(
        audits_with_action(&h, auth::INVITE_REDEEMED_AUDIT_ACTION).len(),
        1
    );
    let replay = h.call("POST", FINISH, finish);
    assert_eq!(replay.status, 401);
    assert!(replay.set_cookie.is_none());
    assert_eq!(h.store.sessions.borrow().len(), 1);
    assert_eq!(
        audits_with_action(&h, auth::INVITE_REDEEMED_AUDIT_ACTION).len(),
        1
    );
    assert_eq!(email_slots(&h, EMAIL), 5);
}

#[test]
fn invalid_starts_share_one_email_pool_across_rotated_clients() {
    let h = setup();
    let clients = [
        "2001:db8:1::1",
        "2001:db8:1::2",
        "2001:db8:2::1",
        "198.51.100.10",
        "198.51.100.11",
    ];
    for (index, ip) in clients.iter().enumerate() {
        assert_eq!(
            h.call_with_ip(
                ip,
                "POST",
                START,
                serde_json::json!({
                    "email": EMAIL, "invite_code": format!("wrong-{index}"),
                })
            )
            .status,
            401
        );
    }
    assert_eq!(email_slots(&h, EMAIL), 5);
    assert_eq!(*h.store.invite_reads.borrow(), 5);
    let denied = h.call_with_ip(
        "2001:db8:3::1",
        "POST",
        START,
        serde_json::json!({"email": EMAIL, "invite_code": CODE}),
    );
    assert_eq!(denied.status, 401);
    assert_eq!(
        *h.store.invite_reads.borrow(),
        5,
        "sixth guess denied before lookup"
    );
    assert_eq!(email_slots(&h, EMAIL), 5);
    let shared = format!("invite:{}:start", rate_limit_ip_key(clients[0]));
    assert_eq!(slots(&h, &shared, "auth.invite_redeem"), 2);
    assert!(h.store.challenges.borrow().is_empty());
    assert!(h.mailer.sent.borrow().is_empty());
}

#[test]
fn independent_starts_are_not_deduplicated() {
    let h = setup();
    let first = start(&h, EMAIL, Some(CODE));
    let second = start(&h, EMAIL, Some(CODE));
    assert_ne!(first["challenge"], second["challenge"]);
    assert_eq!(email_slots(&h, EMAIL), 2);
    assert_eq!(
        slots(
            &h,
            &format!("invite:{}:start", rate_limit_ip_key(IP)),
            "auth.invite_redeem"
        ),
        2
    );
    assert_eq!(h.store.challenges.borrow().len(), 2);
    assert!(h
        .store
        .challenges
        .borrow()
        .values()
        .all(|row| row.invite_budget_receipt.is_some()));
    assert!(!h.store.invites.borrow()[&auth::hash_invite_code(CODE)].used);
}

#[test]
fn changed_or_missing_code_gets_no_credit_and_cannot_replace_receipt() {
    for code in [Some("different-code"), None] {
        let h = setup();
        let (value, mut finish) = admitted(&h);
        burn_email(&h, EMAIL);
        let id = value["challenge"].as_str().unwrap();
        let receipt = h.store.challenges.borrow()[id]
            .invite_budget_receipt
            .clone();
        let reads = *h.store.invite_reads.borrow();
        finish["invite_code"] = serde_json::json!(code);
        let denied = h.call("POST", FINISH, finish.clone());
        assert_eq!(denied.status, 401);
        assert_eq!(*h.store.invite_reads.borrow(), reads);
        assert_eq!(email_slots(&h, EMAIL), 5);
        assert_eq!(
            h.store.challenges.borrow()[id].invite_budget_receipt,
            receipt
        );
        assert_unclaimed(&h, &value, &finish);
        finish["invite_code"] = serde_json::json!(CODE);
        assert_eq!(
            h.call("POST", FINISH, finish).status,
            200,
            "original paid tuple remains usable"
        );
        assert_eq!(email_slots(&h, EMAIL), 5);
    }
}

#[test]
fn paid_receipt_on_one_challenge_never_credits_another_challenge() {
    let h = setup();
    let (paid_start, paid_finish) = admitted(&h);
    let other_start = start(&h, EMAIL, None);
    assert_ne!(paid_start["challenge"], other_start["challenge"]);
    assert_eq!(paid_start["user_id"], other_start["user_id"]);
    burn_email(&h, EMAIL);
    let other_id = other_start["challenge"].as_str().unwrap();
    let body = fixture_body(
        other_id,
        paid_finish["magic_token"].as_str().unwrap(),
        Some(CODE),
    );
    let reads = *h.store.invite_reads.borrow();
    let pairs = *h.store.rate_limit_pair_calls.borrow();
    assert_eq!(h.call("POST", FINISH, body).status, 401);
    assert_eq!(*h.store.invite_reads.borrow(), reads);
    assert_eq!(*h.store.rate_limit_pair_calls.borrow(), pairs + 1);
    assert!(h.store.challenges.borrow()[other_id]
        .invite_budget_receipt
        .is_none());
    assert_eq!(email_slots(&h, EMAIL), 5);
    assert_eq!(h.call("POST", FINISH, paid_finish).status, 200);
    assert_eq!(email_slots(&h, EMAIL), 5);
}

#[test]
fn legacy_finish_and_starts_share_the_same_email_pool() {
    let h = setup();
    let value = start(&h, EMAIL, None);
    let id = value["challenge"].as_str().unwrap();
    assert!(h.store.challenges.borrow()[id]
        .invite_budget_receipt
        .is_none());
    let token = h.mailer.sent.borrow().last().unwrap().1.clone();
    for index in 0..2 {
        assert_eq!(
            h.call_with_ip(
                "2001:db8:1::1",
                "POST",
                START,
                serde_json::json!({
                    "email": EMAIL, "invite_code": format!("start-wrong-{index}"),
                })
            )
            .status,
            401
        );
    }
    for index in 0..3 {
        let body = fixture_body(id, &token, Some(&format!("finish-wrong-{index}")));
        assert_eq!(
            h.call_with_ip("2001:db8:2::1", "POST", FINISH, body).status,
            401
        );
    }
    assert_eq!(email_slots(&h, EMAIL), 5);
    let reads = *h.store.invite_reads.borrow();
    assert_eq!(
        h.call_with_ip(
            "198.51.100.77",
            "POST",
            FINISH,
            fixture_body(id, &token, Some(CODE))
        )
        .status,
        401
    );
    assert_eq!(*h.store.invite_reads.borrow(), reads);
    assert!(!h.store.challenges.borrow()[id].challenge.consumed);
    assert!(h.store.credentials.borrow().is_empty());
}

#[test]
fn changed_email_or_key_and_null_legacy_receipt_use_fresh_pair() {
    for case in ["email", "rotation", "legacy"] {
        let mut h = setup();
        let (value, finish) = admitted(&h);
        let id = value["challenge"].as_str().unwrap();
        let original = h.store.challenges.borrow()[id]
            .invite_budget_receipt
            .clone();
        let target_email = if case == "email" {
            "other@example.com"
        } else {
            EMAIL
        };
        if case == "email" {
            // Controlled stored proof reaches accounting with the same user
            // but a different proven mailbox. No client receipt is involved.
            let magic_hash = auth::hash_magic_token(finish["magic_token"].as_str().unwrap());
            h.store
                .magic
                .borrow_mut()
                .get_mut(&magic_hash)
                .unwrap()
                .email = target_email.to_string();
        } else if case == "rotation" {
            h.config.session_secret = vec![0xa5; 32];
        } else {
            h.store
                .challenges
                .borrow_mut()
                .get_mut(id)
                .unwrap()
                .invite_budget_receipt = None;
        }
        burn_email(&h, target_email);
        let reads = *h.store.invite_reads.borrow();
        let pairs = *h.store.rate_limit_pair_calls.borrow();
        let denied = h.call("POST", FINISH, finish.clone());
        assert_eq!(denied.status, 401, "{case}");
        assert_eq!(
            *h.store.invite_reads.borrow(),
            reads,
            "{case}: before lookup"
        );
        assert_eq!(
            *h.store.rate_limit_pair_calls.borrow(),
            pairs + 1,
            "{case}: ordinary pair"
        );
        assert_eq!(email_slots(&h, target_email), 5);
        assert_unclaimed(&h, &value, &finish);
        if case != "legacy" {
            assert_eq!(
                h.store.challenges.borrow()[id].invite_budget_receipt,
                original
            );
        }
    }
}

#[test]
fn receipt_cannot_move_to_another_challenge_user_or_purpose() {
    for case in [
        "challenge",
        "user",
        "login",
        "reauth",
        "expired",
        "consumed",
        "rp",
        "origin",
    ] {
        let h = setup();
        let (value, finish) = admitted(&h);
        let id = value["challenge"].as_str().unwrap();
        burn_email(&h, EMAIL);
        let admitted_user = h.store.challenges.borrow()[id].user_id.clone().unwrap();
        {
            let mut rows = h.store.challenges.borrow_mut();
            let row = rows.get_mut(id).unwrap();
            match case {
                "challenge" => row.challenge.id = "x".repeat(43),
                "user" => row.user_id = Some("issuer".to_string()),
                "login" => row.purpose = PURPOSE_LOGIN.to_string(),
                "reauth" => row.purpose = PURPOSE_REAUTH.to_string(),
                "expired" => row.challenge.expires_at = NOW,
                "consumed" => row.challenge.consumed = true,
                "rp" => row.challenge.rp_id = "other.example.com".to_string(),
                "origin" => row.challenge.origin = "https://other.example.com".to_string(),
                _ => unreachable!(),
            }
            assert!(
                !row.admits_invite_email(
                    &h.config,
                    id,
                    admitted_user.as_str(),
                    Some(CODE),
                    &email_rate_limit_key(&h.config, EMAIL),
                    NOW
                ),
                "{case}"
            );
        }
        let reads = *h.store.invite_reads.borrow();
        assert_eq!(h.call("POST", FINISH, finish).status, 401, "{case}");
        assert_eq!(*h.store.invite_reads.borrow(), reads, "{case}");
        assert_eq!(email_slots(&h, EMAIL), 5);
        assert!(h.store.credentials.borrow().is_empty());
        assert!(h.store.sessions.borrow().is_empty());
    }
}

#[test]
fn client_supplied_receipt_fields_are_refused_before_admission() {
    let h = setup();
    let mut request = serde_json::json!({"email": EMAIL, "invite_code": CODE, "prechecked": true});
    assert_eq!(h.call("POST", START, request.clone()).status, 401);
    request.as_object_mut().unwrap().remove("prechecked");
    request["invite_budget_receipt"] = serde_json::json!({"invite_code_hash": auth::hash_invite_code(CODE), "email_budget_key": email_rate_limit_key(&h.config, EMAIL)});
    assert_eq!(h.call("POST", START, request).status, 401);
    assert_eq!(email_slots(&h, EMAIL), 0);
    assert_eq!(*h.store.invite_reads.borrow(), 0);
    let (_, mut finish) = admitted(&h);
    finish["prechecked"] = serde_json::json!(true);
    assert_eq!(h.call("POST", FINISH, finish).status, 401);
    assert_eq!(email_slots(&h, EMAIL), 1);
    assert_eq!(*h.store.invite_reads.borrow(), 1);
    assert!(h.store.credentials.borrow().is_empty());
}

#[test]
fn credited_finish_still_obeys_client_lookup_and_cpu_budgets() {
    for action in [
        "auth.invite_redeem",
        "auth.register_finish_ip",
        "auth.register_finish",
    ] {
        let h = setup();
        let (value, finish) = admitted(&h);
        let id = value["challenge"].as_str().unwrap();
        let user = h.store.challenges.borrow()[id].user_id.clone().unwrap();
        let (scope, limit, window) = match action {
            "auth.invite_redeem" => (
                format!("invite:{}:{user}", rate_limit_ip_key(IP)),
                u64::from(auth::INVITE_REDEMPTION_MAX_ATTEMPTS),
                auth::INVITE_REDEMPTION_WINDOW_SECS,
            ),
            "auth.register_finish_ip" => (
                rate_limit_ip_key(IP),
                FINISH_LOOKUP_ATTEMPTS_PER_IP,
                FINISH_ATTEMPT_WINDOW_SECS,
            ),
            _ => (
                id.to_string(),
                REGISTER_FINISH_ATTEMPTS_PER_CHALLENGE,
                FINISH_ATTEMPT_WINDOW_SECS,
            ),
        };
        if action == "auth.invite_redeem" {
            for index in 0..limit {
                let challenge =
                    auth::new_registration_challenge(&h.config.rp_id, ORIGIN, NOW).unwrap();
                block_on(h.store.insert_challenge(
                    &challenge,
                    Some(user.as_str()),
                    PURPOSE_REGISTER,
                    None,
                ))
                .unwrap();
                let other_email = format!("client-budget-{index}@example.com");
                let (token, magic) =
                    auth::issue_magic_link(user.as_str(), &other_email, NOW).unwrap();
                block_on(h.store.insert_magic(&magic)).unwrap();
                let body = fixture_body(&challenge.id, &token, Some("client-budget-wrong"));
                assert_eq!(h.call("POST", FINISH, body).status, 401);
                assert_eq!(email_slots(&h, &other_email), 1);
            }
        } else {
            for _ in 0..limit {
                assert!(block_on(
                    h.store
                        .reserve_rate_limit(&scope, action, limit, NOW, window)
                )
                .unwrap());
            }
        }
        assert_eq!(slots(&h, &scope, action), limit as usize);
        let reads = *h.store.invite_reads.borrow();
        let challenges = *h.store.challenge_reads.borrow();
        let denied = h.call("POST", FINISH, finish.clone());
        assert_eq!(denied.status, 401, "{action}");
        assert_eq!(*h.store.invite_reads.borrow(), reads);
        if action == "auth.register_finish_ip" {
            assert_eq!(*h.store.challenge_reads.borrow(), challenges);
        }
        assert_eq!(
            email_slots(&h, EMAIL),
            1,
            "client refusal never takes another email slot"
        );
        assert_unclaimed(&h, &value, &finish);
    }
}

#[test]
fn credited_finish_rechecks_every_invite_predicate() {
    for case in ["revoked", "expired", "spent", "email", "self"] {
        let h = setup();
        let (value, finish) = admitted(&h);
        burn_email(&h, EMAIL);
        let challenge_id = value["challenge"].as_str().unwrap();
        let internal_user = h.store.challenges.borrow()[challenge_id]
            .user_id
            .clone()
            .unwrap();
        let hash = auth::hash_invite_code(CODE);
        {
            let mut invites = h.store.invites.borrow_mut();
            let invite = invites.get_mut(&hash).unwrap();
            match case {
                "revoked" => invite.revoked_at = Some(NOW),
                "expired" => invite.expires_at = NOW,
                "spent" => {
                    invite.used = true;
                    invite.redeemed_at = Some(NOW);
                }
                "email" => invite.email = Some("not-the-mailbox@example.com".to_string()),
                "self" => invite.issued_by = Some(internal_user.clone()),
                _ => unreachable!(),
            }
        }
        let reads = *h.store.invite_reads.borrow();
        assert_eq!(h.call("POST", FINISH, finish.clone()).status, 401, "{case}");
        assert_eq!(
            *h.store.invite_reads.borrow(),
            reads + 1,
            "receipt never replaces lookup"
        );
        assert_eq!(email_slots(&h, EMAIL), 5);
        assert!(
            !h.store.challenges.borrow()[value["challenge"].as_str().unwrap()]
                .challenge
                .consumed
        );
        assert!(
            !h.store.magic.borrow()
                [&auth::hash_magic_token(finish["magic_token"].as_str().unwrap())]
                .used
        );
        assert!(h.store.credentials.borrow().is_empty());
        assert!(h.store.sessions.borrow().is_empty());
        assert!(audits_with_action(&h, auth::INVITE_REDEEMED_AUDIT_ACTION).is_empty());
    }
}

#[test]
fn credited_transaction_failure_rolls_back_and_retries_without_email_debit() {
    for fault in ["credential", "audit"] {
        let h = setup();
        let (value, finish) = admitted(&h);
        burn_email(&h, EMAIL);
        let id = value["challenge"].as_str().unwrap();
        let receipt = h.store.challenges.borrow()[id]
            .invite_budget_receipt
            .clone();
        let credential_id = finish["credential_id"].as_str().unwrap();
        if fault == "credential" {
            block_on(h.store.insert_credential(&PasskeyCredential {
                cred_id_b64url: credential_id.to_string(),
                user_id: "issuer".to_string(),
                public_key: fx_es256_cose(92),
                sign_count: 0,
                registered_at: NOW,
            }))
            .unwrap();
        } else {
            *h.store.fail_invite_audit.borrow_mut() = true;
        }
        assert_eq!(
            h.call("POST", FINISH, finish.clone()).status,
            401,
            "{fault}"
        );
        if fault == "credential" {
            h.store.credentials.borrow_mut().remove(credential_id);
        }
        *h.store.fail_invite_audit.borrow_mut() = false;
        assert_unclaimed(&h, &value, &finish);
        assert_eq!(
            h.store.challenges.borrow()[id].invite_budget_receipt,
            receipt
        );
        assert_eq!(email_slots(&h, EMAIL), 5);
        let retry = h.call("POST", FINISH, finish.clone());
        assert_eq!(retry.status, 200, "{fault}: {}", retry.body);
        assert!(retry.set_cookie.is_some());
        assert_eq!(email_slots(&h, EMAIL), 5);
        assert_eq!(
            audits_with_action(&h, auth::INVITE_REDEEMED_AUDIT_ACTION).len(),
            1
        );
        assert_eq!(h.call("POST", FINISH, finish).status, 401);
        assert_eq!(h.store.credentials.borrow().len(), 1);
        assert_eq!(h.store.sessions.borrow().len(), 1);
    }
}

#[test]
fn wrong_inbox_proof_preserves_receipt_and_original_paid_tuple() {
    let h = setup();
    let (value, finish) = admitted(&h);
    burn_email(&h, EMAIL);
    let id = value["challenge"].as_str().unwrap();
    let receipt = h.store.challenges.borrow()[id]
        .invite_budget_receipt
        .clone();
    let mut wrong = finish.clone();
    wrong["magic_token"] = serde_json::json!("wrong-proof");
    assert_eq!(h.call("POST", FINISH, wrong).status, 401);
    assert_eq!(*h.store.invite_reads.borrow(), 1);
    assert_eq!(email_slots(&h, EMAIL), 5);
    assert_eq!(
        h.store.challenges.borrow()[id].invite_budget_receipt,
        receipt
    );
    assert_unclaimed(&h, &value, &finish);
    assert_eq!(h.call("POST", FINISH, finish).status, 200);
    assert_eq!(email_slots(&h, EMAIL), 5);
}

#[test]
fn concurrent_reap_healing_keeps_the_exact_receipt_on_every_insert_path() {
    for race in ["before", "cascade", "orphan"] {
        let h = setup();
        let user_id = "resuming";
        block_on(h.store.insert_user(&UserRecord {
            id: user_id.to_string(),
            email: EMAIL.to_string(),
            created_at: NOW.saturating_sub(UNVERIFIED_USER_TTL_SECS + 1),
        }))
        .unwrap();
        match race {
            "before" => {
                *h.store.delete_user_before_next_challenge.borrow_mut() = Some(user_id.to_string())
            }
            "cascade" => {
                *h.store.delete_user_after_next_challenge.borrow_mut() = Some(user_id.to_string())
            }
            _ => *h.store.orphan_user_after_next_challenge.borrow_mut() = Some(user_id.to_string()),
        }
        let (value, finish) = admitted(&h);
        let id = value["challenge"].as_str().unwrap();
        assert_eq!(
            value["user_id"].as_str().unwrap(),
            fake_user_id(&h.config, EMAIL)
        );
        assert_eq!(
            h.store.challenges.borrow()[id].user_id.as_deref(),
            Some(user_id)
        );
        assert_eq!(
            h.store.challenges.borrow()[id].invite_budget_receipt,
            Some(InviteBudgetReceipt {
                invite_code_hash: auth::hash_invite_code(CODE),
                email_budget_key: email_rate_limit_key(&h.config, EMAIL),
            }),
            "{race}"
        );
        assert_eq!(email_slots(&h, EMAIL), 1);
        assert_eq!(h.call("POST", FINISH, finish).status, 200, "{race}");
        assert_eq!(email_slots(&h, EMAIL), 1);
    }
}
