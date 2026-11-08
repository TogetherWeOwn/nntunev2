// Kept independent of the receipt type so this same routed regression can
// execute on the original handler/storage implementation as red-before proof.
#[test]
fn corrected_fifth_invite_guess_can_complete_registration() {
    let mut h = harness();
    h.config.invites_required = true;
    let email = "boundary@example.com";
    let code = "boundary-valid-invite";
    let hash = auth::hash_invite_code(code);
    h.store.invites.borrow_mut().insert(
        hash.clone(),
        InviteRecord::new_issued(
            hash.clone(),
            None,
            auth::INVITE_KIND_ADMIN,
            None,
            NOW,
            NOW + 604_800,
        ),
    );
    let start_path = "/v1/auth/passkey/register/start";
    let mut refusals = Vec::new();
    for index in 0..4 {
        let response = h.call(
            "POST",
            start_path,
            serde_json::json!({
                "email": email, "invite_code": format!("wrong-invite-{index}"),
            }),
        );
        assert_eq!(response.status, 401, "invalid start");
        refusals.push(response.body);
        assert!(h.store.users.borrow().is_empty());
        assert!(h.store.challenges.borrow().is_empty());
        assert!(h.mailer.sent.borrow().is_empty());
        assert!(!h.store.invites.borrow()[&hash].used);
    }
    assert!(refusals.iter().all(|body| body == &refusals[0]));
    let start = h.call(
        "POST",
        start_path,
        serde_json::json!({"email": email, "invite_code": code}),
    );
    assert_eq!(start.status, 200, "valid fifth start: {}", start.body);
    assert!(!h.store.invites.borrow()[&hash].used);
    let value: serde_json::Value = serde_json::from_str(&start.body).unwrap();
    let challenge = value["challenge"].as_str().unwrap();
    let token = h.mailer.sent.borrow().last().unwrap().1.clone();
    let cred_id = b"boundary-credential";
    let att = fx_none_attestation(cred_id, &fx_es256_cose(93));
    let email_count = || {
        let key = email_rate_limit_key(&h.config, email);
        h.store
            .rate_limit_slots
            .borrow()
            .iter()
            .filter(|((scope, action, _), expiry)| {
                scope == &key && action == "auth.invite_redeem_email" && **expiry > NOW
            })
            .count()
    };
    assert_eq!(email_count(), 5);
    let finish = h.call("POST", "/v1/auth/passkey/register/finish", serde_json::json!({
        "challenge": challenge,
        "credential_id": b64url_encode(cred_id),
        "attestation_object": b64url_encode(&att),
        "client_data_json": b64url_encode(fx_client_json("webauthn.create", challenge).as_bytes()),
        "magic_token": token,
        "invite_code": code,
    }));
    assert_eq!(
        finish.status, 200,
        "a valid fifth guess must not need a sixth email slot: {}",
        finish.body
    );
    assert!(finish.set_cookie.is_some());
    assert_eq!(
        email_count(),
        5,
        "completion neither refunds nor double-charges the guess"
    );
    assert_eq!(h.store.credentials.borrow().len(), 1);
    assert_eq!(h.store.sessions.borrow().len(), 1);
    assert!(h.store.challenges.borrow()[challenge].challenge.consumed);
    assert!(h.store.magic.borrow()[&auth::hash_magic_token(&token)].used);
    assert!(h.store.invites.borrow()[&hash].used);
    assert_eq!(
        audits_with_action(&h, auth::INVITE_REDEEMED_AUDIT_ACTION).len(),
        1
    );
}
