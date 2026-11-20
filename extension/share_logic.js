// Pure helpers for the share-image flow.

// The Priority copy button shows only on the Priority tab and when it has rows.
export function priorityCopyVisible(tab, groups) {
    return tab === 'priority' && (groups ?? []).some(g => g.rows?.length > 0);
}

// The string parameter of the app's share-image action.
export function shareRequest(kind, rowId, requestId) {
    const req = {kind, requestId};
    if (kind === 'breakdown')
        req.rowId = rowId;
    return JSON.stringify(req);
}
