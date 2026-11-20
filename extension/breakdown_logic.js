// What a history row's breakdown shows. Pure (no gi imports), so it runs under node or gjs.

export function headerText(b) {
    return [b.title, ...(b.figures ?? [])].join(' · ');
}

export function emptyText(provider) {
    return provider ? `${provider} didn't report any data for this range.` : "No data was reported for this range.";
}

// A bar's fill, clamped to 0..1; null when the part carries no fraction.
export function barFraction(part) {
    return typeof part.fraction === 'number' ? Math.min(1, Math.max(0, part.fraction)) : null;
}

// Which history rows are open, kept between redraws. A row is told apart by its account's card id and title.
export class BreakdownOpen {
    constructor() { this._open = new Set(); }

    static key(row) { return JSON.stringify([row.card_id ?? '', row.title]); }

    isOpen(key) { return this._open.has(key); }

    toggle(key) {
        if (!this._open.delete(key))
            this._open.add(key);
    }
}
