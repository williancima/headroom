import Clutter from 'gi://Clutter';
import St from 'gi://St';
import {vbox} from './layout.js';
import {GENERAL_DETAIL, entryKeys} from './resets_logic.js';

function label(text, style) {
    const l = new St.Label({text, style_class: style, x_expand: true});
    l.clutter_text.line_wrap = true;
    return l;
}

function button(text, style, onClick) {
    const b = new St.Button({label: text, style_class: style, can_focus: true, y_align: Clutter.ActorAlign.CENTER});
    b.connect('clicked', onClick);
    return b;
}

// The list under an expanded "Rate Limit Resets" card. `onChange` redraws; `onClaim(key, entry, requestId)` sends the claim.
export function resetsList(flow, cardId, entries, onChange, onClaim) {
    const box = vbox({x_expand: true, style_class: 'headroom-resets'});
    const message = flow.message(cardId);
    if (message)
        box.add_child(label(message, 'headroom-resets-message'));
    const keys = entryKeys(cardId, entries);
    entries.forEach((entry, i) => {
        const key = keys[i];
        const step = flow.step(key);
        const row = vbox({x_expand: true, style_class: 'headroom-reset'});
        const top = new St.BoxLayout({x_expand: true});
        const texts = vbox({x_expand: true});
        texts.add_child(new St.Label({text: entry.title, style_class: 'headroom-card-title'}));
        texts.add_child(label(entry.expires, 'headroom-card-subtitle'));
        top.add_child(texts);
        if (step === null && entry.usable) {
            top.add_child(button('Use', 'headroom-button', () => {
                flow.ask(key);
                onChange();
            }));
        } else if (step === 'claiming') {
            top.add_child(new St.Label({text: 'Using…', style_class: 'headroom-card-subtitle', y_align: Clutter.ActorAlign.CENTER}));
        }
        row.add_child(top);
        if (step === 'confirming') {
            row.add_child(new St.Label({text: 'Use This Reset?', style_class: 'headroom-card-title'}));
            row.add_child(label(entry.confirmDetail ?? GENERAL_DETAIL, 'headroom-card-subtitle'));
            const buttons = new St.BoxLayout({x_expand: true, style_class: 'headroom-reset-buttons'});
            buttons.add_child(button('Cancel', 'headroom-button', () => {
                flow.cancel(key);
                onChange();
            }));
            buttons.add_child(button('Use', 'headroom-button headroom-button-primary', () => {
                const id = flow.confirm(key);
                if (id)
                    onClaim(key, entry, id);
                onChange();
            }));
            row.add_child(buttons);
        }
        box.add_child(row);
    });
    return box;
}
