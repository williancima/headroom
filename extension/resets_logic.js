// What the dropdown remembers about resets between redraws. Pure (no gi imports), so it runs under node or gjs.
// Mirrors crates/headroom-app/src/resets_logic.rs.

export const GENERAL_DETAIL = "Immediately reset your usage limits. This can't be undone.";

// Identical resets (Claude repeats a grant per reset left) are told apart by their position among equals.
export function entryKeys(cardId, entries) {
    const seen = new Map();
    return entries.map(e => {
        const claim = JSON.stringify(e.claim);
        const nth = seen.get(claim) ?? 0;
        seen.set(claim, nth + 1);
        return JSON.stringify([e.cardId ?? cardId, claim, nth]); // a unified row's entries each name their account
    });
}

// Every key the rows of a panel tab hold.
export function liveKeys(groups) {
    const live = new Set();
    for (const g of groups ?? []) {
        for (const r of g.rows ?? []) {
            if (r.card_id && r.resets?.length)
                entryKeys(r.card_id, r.resets).forEach(k => live.add(k));
        }
    }
    return live;
}

export class ResetFlow {
    constructor(newId) {
        this._newId = newId;
        this._open = new Set();
        this._steps = new Map();      // key -> 'confirming' | 'claiming'
        this._requestIds = new Map(); // card id + claim -> id, kept until the claim settles
        this._claiming = new Map();   // request id -> key
        this._messages = new Map();   // card id -> last outcome
    }

    isOpen(cardId) { return this._open.has(cardId); }

    toggle(cardId) {
        if (!this._open.delete(cardId))
            this._open.add(cardId);
    }

    step(key) { return this._steps.get(key) ?? null; }

    message(cardId) { return this._messages.get(cardId) ?? null; }

    _idKey(key) {
        const [cardId, claim] = JSON.parse(key);
        return `${cardId}\n${claim}`;
    }

    ask(key) {
        if (this._steps.has(key))
            return;
        const idKey = this._idKey(key);
        if (!this._requestIds.has(idKey))
            this._requestIds.set(idKey, this._newId());
        this._messages.delete(JSON.parse(key)[0]);
        this._steps.set(key, 'confirming');
    }

    cancel(key) {
        if (this._steps.get(key) === 'confirming')
            this._steps.delete(key);
    }

    // Returns the request id to send, or null when there is nothing to confirm or an identical reset is in flight.
    confirm(key) {
        if (this._steps.get(key) !== 'confirming')
            return null;
        const id = this._requestIds.get(this._idKey(key));
        if (!id || this._claiming.has(id))
            return null;
        this._steps.set(key, 'claiming');
        this._claiming.set(id, key);
        return id;
    }

    // The claim answered. A settled one retires its request id; an unsettled one goes back to the confirm
    // under the same id, so a retry can't spend a second reset. Returns the card id, or null if unknown.
    finish(requestId, message, settled) {
        const key = this._claiming.get(requestId);
        if (!key)
            return null;
        this._claiming.delete(requestId);
        const cardId = JSON.parse(key)[0];
        this._messages.set(cardId, message);
        if (settled) {
            this._steps.delete(key);
            this._requestIds.delete(this._idKey(key));
        } else {
            this._steps.set(key, 'confirming');
        }
        return cardId;
    }

    // A reload that drops a row cancels its confirm; a claim in flight stays.
    reconcile(live) {
        for (const [key, step] of [...this._steps]) {
            if (step === 'confirming' && !live.has(key))
                this._steps.delete(key);
        }
    }
}

// The `claim-reset` action's parameter, as crates/headroom-app/src/resets_logic.rs parses it.
export function claimParameter(key, claim, requestId) {
    return JSON.stringify({card_id: JSON.parse(key)[0], claim, request_id: requestId});
}
