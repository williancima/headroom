import Clutter from 'gi://Clutter';
import St from 'gi://St';
import {meter, vbox} from './layout.js';
import {barFraction, emptyText, headerText} from './breakdown_logic.js';

function label(text, style) {
    const l = new St.Label({text, style_class: style, x_expand: true});
    l.clutter_text.line_wrap = true;
    return l;
}

function line(name, value, style) {
    const row = new St.BoxLayout({x_expand: true});
    row.add_child(new St.Label({text: name, style_class: style, x_expand: true}));
    row.add_child(new St.Label({text: value, style_class: style}));
    return row;
}

// The panel under an open history row: header, token parts (with bars), then Top Models.
export function breakdownPanel(b, provider, onCopy = null) {
    const box = vbox({x_expand: true, style_class: 'headroom-breakdown'});
    box.add_child(label(headerText(b), 'headroom-card-title'));
    if (onCopy && !b.empty) {
        const copy = new St.Button({label: 'Copy', style_class: 'headroom-button', can_focus: true, x_align: Clutter.ActorAlign.START});
        copy.connect('clicked', onCopy);
        box.add_child(copy);
    }
    if (b.empty) {
        box.add_child(label(emptyText(provider), 'headroom-card-subtitle'));
        return box;
    }
    for (const part of b.token_parts ?? []) {
        box.add_child(line(part.title, part.value, 'headroom-card-subtitle'));
        const frac = barFraction(part);
        if (frac !== null)
            box.add_child(meter(frac));
    }
    if (b.models?.length) {
        box.add_child(new St.Label({text: 'Top Models', style_class: 'headroom-card-title headroom-breakdown-heading'}));
        if (b.models_label)
            box.add_child(label(b.models_label, 'headroom-card-subtitle'));
        for (const m of b.models)
            box.add_child(line(m.name, m.value, 'headroom-card-subtitle'));
    }
    return box;
}
