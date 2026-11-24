// What the customization menus and drags need, as plain data. Pure (no gi imports), so it runs under node or gjs.
// Mirrors crates/headroom-app/src/layout_logic.rs.

// What each row of a group is, for the drags: {type: 'header', cardId}, {type: 'metric', cardId | null} or null.
export function roles(group) {
    let account = null;
    return (group.rows ?? []).map(row => {
        const id = row.id ?? null;
        const cardId = row.card_id ?? null;
        if (id === null && cardId !== null && !row.resets?.length && !row.breakdown) {
            account = cardId;
            return {type: 'header', cardId};
        }
        if (id !== null)
            return {type: 'metric', cardId: group.unified || row.members?.length ? null : account};
        return null;
    });
}

// The line ids Pin and Hide act on: every account's copy on a unified row; null on a row that is no metric.
export function menuIds(row) {
    if (!row.id)
        return null;
    return row.copy_ids?.length ? row.copy_ids : [row.id];
}

// A metric's label is what follows its account in the line id ("claude@1a2b3c4d/Weekly" -> "Weekly").
export function metricLabel(id, cardId) {
    return id.startsWith(`${cardId}/`) ? id.slice(cardId.length + 1) : id;
}

// What a drag carries: {type: 'metric', cardId, label} | {type: 'account', cardId} | {type: 'provider', kind}.
// The [action, signature, args] a drop on `target` calls; null across types, or across accounts for metrics.
export function dropAction(dragged, target) {
    if (dragged.type !== target.type)
        return null;
    switch (dragged.type) {
    case 'metric':
        return dragged.cardId === target.cardId ? ['move-metric', '(sss)', [dragged.cardId, dragged.label, target.label]] : null;
    case 'account':
        return ['move-account', '(ss)', [dragged.cardId, target.cardId]];
    case 'provider':
        return ['move-provider', '(ss)', [dragged.kind, target.kind]];
    default:
        return null;
    }
}

// Where the extension ships a provider's icon (`kind` is the provider id the panel JSON carries).
export function providerIconFile(kind) {
    return `icons/headroom-provider-${kind}-symbolic.svg`;
}

// The top bar: the Headroom icon alone when nothing is running out, else each item's provider icon and text, then `+N`.
export function barParts(data) {
    const items = data?.bar ?? [];
    if (!items.length)
        return [{icon: 'icons/dev.headroom.Headroom-symbolic.svg'}];
    const parts = items.map(i => ({icon: providerIconFile(i.kind), text: i.text}));
    if (data.bar_overflow)
        parts.push({text: `+${data.bar_overflow}`});
    return parts;
}

// Where a tooltip goes: centered under the button (above it near the screen's bottom), inside the monitor.
export function tooltipPosition(button, label, monitor, gap = 6) {
    const x = Math.round(button.x + (button.width - label.width) / 2);
    let y = button.y + button.height + gap;
    if (y + label.height > monitor.y + monitor.height)
        y = button.y - label.height - gap;
    return {x: Math.max(monitor.x, Math.min(x, monitor.x + monitor.width - label.width)), y: Math.round(y)};
}
