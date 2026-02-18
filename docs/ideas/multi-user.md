# Multi-User Support

Per-user isolation with invite-only registration.

## User Model

```
user {
    id: UUID
    username: String
    password_hash: String
    role: "admin" | "user"     # admin can manage users, view all stats
    timezone: String           # e.g., "Europe/Kyiv" — used for schedule interpretation
    feed_token: String         # per-user token for feed authentication
    tg_session: Option<Bytes>  # grammers session data (encrypted at rest)
    invited_by: Option<UUID>   # which admin/invite created this account
    created_at: DateTime
}

invite {
    id: UUID
    token: String              # single-use, random, URL-safe
    created_by: UUID           # admin who generated it
    used_by: Option<UUID>      # user who consumed it (NULL if unused)
    expires_at: Option<DateTime>
    created_at: DateTime
}
```

## Invite-Only Registration

No open sign-up. Rationale: pail gives each user near-direct access to opencode (a powerful agentic LLM tool) via their editorial prompts. Uncontrolled sign-up would be a prompt injection and resource abuse vector.

**Invite flow:**
1. Admin generates an invite token via web UI or CLI (`pail invite create`)
2. Admin shares the token (or a registration URL containing it) with the invitee
3. Invitee visits the registration page, enters the token, chooses username + password
4. Token is consumed (single-use) and the account is created

## Admin Visibility

The admin can view all users and per-user stats (prompt size, number of output channels, number of sources per channel, generation history) to detect unusual activity or resource abuse. Admin can disable or delete user accounts.

## Source Ownership

Per-user isolated source lists. Two users can independently add the same RSS URL. No shared source pool.

## Per-User Telegram Sessions

Each user logs in to Telegram via the web UI (instead of CLI). Session data encrypted at rest.

## Dependencies

- Requires [Web UI](web-ui.md) for the registration/login/management interface
- Feed URLs change from `/feed/default/...` to `/feed/<username>/...`

## Decisions

- **Registration model:** invite-only, no open sign-up.
  Options: open sign-up / invite-only / admin-created accounts only.
  Rationale: pail gives each user near-direct access to opencode via editorial prompts. Uncontrolled sign-up would be a prompt injection and resource abuse vector.

- **Source ownership:** per-user isolated lists, no shared source pool.
  Options: shared source pool / per-user isolated / shared with per-user overrides.
  Rationale: simplest model. Two users can independently add the same RSS URL without conflict.
