# Marketplace TODO

As of March 11, 2026, the `kabosu` marketplace backend has working auth challenges/sessions, trader profiles/activity, listings, offers, auction creation, auction bids, auction bid cancellation, auction settlement, signed-intent verification, and RPC-backed transaction broadcast/status checks.

This file tracks what is still open, ordered from easier follow-ups to harder systems work.

## Current Baseline

- Auth endpoints exist: `POST /v1/auth/challenge`, `POST /v1/auth/verify`.
- Trader endpoints exist: `GET /v1/traders/:address`, `PATCH /v1/traders/:address`, `POST /v1/traders/:address/x/verify`, `GET /v1/traders/:address/activity`.
- Listing endpoints exist: list/detail/create/cancel/build/submit/status.
- Offer endpoints exist: list/create/cancel.
- Auction endpoints exist: list/detail/create/bid/bid-cancel/settle.
- Signed intents are currently enforced for:
  - `offer_create`
  - `listing_buy` submit
  - `bid_place`
  - `auction_settle`

## TODO

### 1. Replace Manual X Verification With Real OAuth2 PKCE

- [ ] Replace `POST /v1/traders/:address/x/verify` with a real X OAuth2 PKCE flow.
- [ ] Stop trusting client-submitted `xHandle` / `xUserId` values directly.
- [ ] Persist OAuth state and callback validation.
- [ ] Only mark `xVerified = true` after X callback success.

Acceptance:
- The backend initiates the X auth flow, validates the callback, and stores verified X identity data server-side.

### 2. Enforce Indexer-Backed Ownership On Listing And Auction Creation

- [ ] Validate inscription ownership before `POST /v1/listings`.
- [ ] Validate inscription ownership before `POST /v1/auctions`.
- [ ] Reject stale or mismatched seller ownership claims based on indexed location state.

Acceptance:
- A seller cannot create a listing or auction for an inscription they no longer control.

### 3. Finish Signed-Intent Coverage For Remaining Economic Actions

- [ ] Add canonical signed-intent validation for offer cancellation (`offer_cancel`) instead of session-only cancellation.
- [ ] Decide whether listing cancellation should remain session-only or also require a signed intent.
- [ ] Normalize all signed marketplace write routes around one envelope shape and one nonce policy.

Acceptance:
- Every marketplace action that moves economic state either uses session auth by design and is documented as such, or is protected by canonical Dogecoin signed intents.

### 4. Upgrade Order Build From Template To Real Transaction Construction

- [ ] Replace the current `build` response template with a real unsigned transaction or PSBT construction flow.
- [ ] Stop overloading `signedPsbt` with raw transaction hex once the final transaction format is chosen.
- [ ] Define one canonical submit format for Dojak and browser-wallet clients.

Acceptance:
- `POST /v1/orders/:listing_id/build` returns something the wallet can sign directly without custom client-side transaction assembly.

### 5. Tighten Settlement Validation Beyond Seller Payment Checks

- [ ] Validate inscription transfer outputs during listing settlement.
- [ ] Validate inscription transfer outputs during auction settlement.
- [ ] Verify the winning buyer/bidder actually receives the inscription in the submitted transaction.
- [ ] Validate fee outputs explicitly, not just seller payment totals.

Acceptance:
- Settlement succeeds only if payment outputs and inscription transfer outputs both match server-side expectations.

### 6. Add Offer Acceptance And Offer Settlement Lifecycle

- [ ] Implement seller-side offer acceptance.
- [ ] Transition offers from `active` to `accepted_pending_settlement` to `fulfilled`.
- [ ] Mirror the listing settlement validation path for accepted offers.
- [ ] Add activity events for acceptance and fulfillment.

Acceptance:
- Offers can complete end-to-end with auditable state transitions.

### 7. Add Confirmation Reconciliation And Finalization Workers

- [ ] Reconcile `settlement_txid` records against Dogecoin RPC in the background.
- [ ] Move listings from `sold_pending_settlement` to finalized state after the confirmation threshold.
- [ ] Reconcile auction settlement confirmation state similarly.
- [ ] Keep `GET /v1/tx/:txid/status` and persisted marketplace state consistent.

Acceptance:
- Marketplace state advances automatically after broadcast instead of requiring clients to poll and infer finality themselves.

### 8. Add Reorg-Aware Rollback / Invalidation

- [ ] Detect reorganizations affecting marketplace settlement transactions.
- [ ] Roll listings/offers/auctions back out of finalized states when chain evidence disappears.
- [ ] Mark invalidated marketplace records explicitly.

Acceptance:
- Terminal marketplace states are reversible when Dogecoin chain history is reorganized.

### 9. Add Request Hardening And Operational Controls

- [ ] Add rate limiting to auth, submit, offer, bid, and settlement routes.
- [ ] Add trace IDs / structured request logging for marketplace routes.
- [ ] Add explicit abuse controls for auth challenge creation and signature verification.

Acceptance:
- Marketplace routes have production-grade request controls and observable request traces.

### 10. Add Marketplace Event Emission

- [ ] Emit webhooks or internal events for:
  - listing created
  - listing sold
  - offer accepted
  - bid placed
  - auction settled
- [ ] Document retry / delivery semantics.

Acceptance:
- External services can react to marketplace state transitions without scraping polling endpoints.

### 11. Add Integration And Migration Testing

- [ ] Add API-level tests for auth challenge -> verify -> session use.
- [ ] Add tests for signed-intent replay rejection.
- [ ] Add tests for listing build -> submit -> confirm lifecycle.
- [ ] Add tests for auction bid race / anti-sniping extension / settlement.
- [ ] Run live migration smoke tests for `V29__marketplace.sql` and `V30__marketplace_auth_bids.sql`.

Acceptance:
- Marketplace flows are covered by repeatable tests instead of compile-only validation.

## Notes

- `POST /v1/orders/:listing_id/submit` currently accepts raw transaction hex in the `signedPsbt` field for API compatibility. This is temporary.
- `GET /v1/tx/:txid/status` now checks RPC confirmations, but no background state reconciler updates marketplace records yet.
- `POST /v1/traders/:address/x/verify` is currently a stateful placeholder endpoint, not a real X OAuth integration.
