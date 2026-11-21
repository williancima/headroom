import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import GObject from 'gi://GObject';
import Pango from 'gi://Pango';
import St from 'gi://St';
import * as DND from 'resource:///org/gnome/shell/ui/dnd.js';
import {dropAction, providerIconFile} from './layout_logic.js';

// A label that wraps instead of ellipsizing.
export function wrap(label) {
    label.clutter_text.line_wrap = true;
    label.clutter_text.ellipsize = Pango.EllipsizeMode.NONE;
    return label;
}

// The provider's icon, left of a card's title; the extension ships the SVGs the app bundles.
export function providerIcon(dir, kind) {
    return new St.Icon({
        gicon: Gio.icon_new_for_string(dir.get_child(providerIconFile(kind)).get_path()),
        icon_size: 16,
        style_class: 'headroom-card-icon',
        y_align: Clutter.ActorAlign.START,
    });
}

// A vertical St.BoxLayout. `vertical: true` is removed in GNOME 51 and `orientation` does not exist before 48, so set
// the orientation on the layout manager, which works on every supported version.
export function vbox(props) {
    const box = new St.BoxLayout(props);
    box.layout_manager.orientation = Clutter.Orientation.VERTICAL;
    return box;
}

// A thin progress bar filled to `fraction` (0 to 1). The track places the fill itself while it is allocated, so the
// first frame already shows the right width (an idle-later resize drew every rebuilt bar empty for a frame, which
// made the menu blink on each refresh). Allocating the child directly queues no relayout, unlike `set_width`.
const Meter = GObject.registerClass(class HeadroomMeter extends St.Widget {
    _init(fraction) {
        super._init({style_class: 'headroom-meter', x_expand: true});
        this._fraction = Math.min(1, Math.max(0, fraction));
        this._fill = new St.Widget({style_class: 'headroom-meter-fill'});
        this.add_child(this._fill);
    }

    vfunc_allocate(box) {
        this.set_allocation(box);
        const content = this.get_theme_node().get_content_box(box);
        const fill = content.copy();
        fill.x2 = fill.x1 + Math.round(content.get_width() * this._fraction);
        this._fill.allocate(fill);
    }
});

export function meter(fraction) {
    return new Meter(fraction);
}

// The tab switch: `tabs` (a row of buttons) over one pill that marks the selected tab. The track allocates the pill
// from the selected tab's allocation in its own pass, so it is in place from the first frame and never feeds back
// into the layout. Switching slides it with `translation_x`, which only moves the paint.
const TabTrack = GObject.registerClass(class HeadroomTabTrack extends St.Widget {
    _init(tabs, selected) {
        super._init({style_class: 'headroom-track', x_expand: true});
        this._tabs = tabs;
        this._selected = selected;
        this._pill = new St.Widget({style_class: 'headroom-tab-pill'});
        this.add_child(this._pill);
        this.add_child(tabs);
    }

    select(tab) {
        if (tab === this._selected)
            return;
        // Start from where the pill is drawn now (also mid-slide), then glide to the new tab.
        const offset = this._pill.translation_x + this._selected.x - tab.x;
        this._selected = tab;
        this._pill.remove_all_transitions();
        this._pill.translation_x = offset;
        this._pill.ease({translation_x: 0, duration: 200, mode: Clutter.AnimationMode.EASE_OUT_QUAD});
        this.queue_relayout();
    }

    vfunc_get_preferred_width(forHeight) {
        const node = this.get_theme_node();
        const [min, nat] = this._tabs.get_preferred_width(node.adjust_for_height(forHeight));
        return node.adjust_preferred_width(min, nat);
    }

    vfunc_get_preferred_height(forWidth) {
        const node = this.get_theme_node();
        const [min, nat] = this._tabs.get_preferred_height(node.adjust_for_width(forWidth));
        return node.adjust_preferred_height(min, nat);
    }

    vfunc_allocate(box) {
        this.set_allocation(box);
        const content = this.get_theme_node().get_content_box(box);
        this._tabs.allocate(content);
        const tab = this._selected.get_allocation_box();
        this._pill.allocate(new Clutter.ActorBox({
            x1: content.x1 + tab.x1, y1: content.y1 + tab.y1, x2: content.x1 + tab.x2, y2: content.y1 + tab.y2,
        }));
    }
});

export function tabTrack(tabs, selected) {
    return new TabTrack(tabs, selected);
}

// Left click runs `primary`, right click `secondary`. Both run after the event, because they redraw the list and the
// press and release must still reach the drag helper on a parent (it lets go of its grab on the release).
export function clickable(actor, primary, secondary = null) {
    actor.reactive = true;
    actor.track_hover = true;
    actor.connect('button-release-event', (_a, event) => {
        const handler = event.get_button() === 3 ? secondary : event.get_button() === 1 ? primary : null;
        if (handler) {
            GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
                handler();
                return GLib.SOURCE_REMOVE;
            });
        }
        return Clutter.EVENT_PROPAGATE;
    });
}

// Makes `actor` draggable and a drop target for the same kind of payload (see layout_logic.js `dropAction`).
// `onDrop(action, signature, args)` sends the move.
export function reorderable(actor, payload, text, onDrop) {
    actor._delegate = {
        payload,
        getDragActor: () => new St.Label({text, style_class: 'headroom-drag'}),
        handleDragOver: source => source?.payload && dropAction(source.payload, payload) ? DND.DragMotionResult.MOVE_DROP : DND.DragMotionResult.NO_DROP,
        acceptDrop: source => {
            const drop = source?.payload ? dropAction(source.payload, payload) : null;
            if (!drop)
                return false;
            onDrop(...drop);
            return true;
        },
    };
    actor.reactive = true;
    DND.makeDraggable(actor, {restoreOnSuccess: true});
}

// The right-click choices, shown in the list under the row (a popup inside the dropdown would fight its grab).
export function choiceBar(choices) {
    const bar = new St.BoxLayout({x_expand: true, style_class: 'headroom-reset-buttons'});
    for (const [text, run] of choices) {
        const b = new St.Button({label: text, style_class: 'headroom-button', can_focus: true});
        b.connect('clicked', run);
        bar.add_child(b);
    }
    return bar;
}

// The panel under an open unified row: one line per account.
export function membersPanel(members) {
    const box = vbox({x_expand: true, style_class: 'headroom-breakdown'});
    for (const m of members) {
        const top = new St.BoxLayout({x_expand: true});
        const texts = vbox({x_expand: true});
        texts.add_child(new St.Label({text: m.title, style_class: 'headroom-card-title'}));
        if (m.subtitle)
            texts.add_child(new St.Label({text: m.subtitle, style_class: 'headroom-card-subtitle'}));
        top.add_child(texts);
        if (m.value)
            top.add_child(new St.Label({text: m.value, style_class: 'headroom-card-value', y_align: Clutter.ActorAlign.START}));
        box.add_child(top);
        if (typeof m.meter === 'number')
            box.add_child(meter(m.meter));
    }
    return box;
}
