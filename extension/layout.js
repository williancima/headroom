import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import St from 'gi://St';
import * as DND from 'resource:///org/gnome/shell/ui/dnd.js';
import {dropAction} from './layout_logic.js';

// A vertical St.BoxLayout. `vertical: true` is removed in GNOME 51 and `orientation` does not exist before 48, so set
// the orientation on the layout manager, which works on every supported version.
export function vbox(props) {
    const box = new St.BoxLayout(props);
    box.layout_manager.orientation = Clutter.Orientation.VERTICAL;
    return box;
}

// A thin progress bar filled to `fraction` (0 to 1). The fill follows the track's width one idle later: resizing it
// inside the track's allocation (notify::width) queues a relayout mid-allocation, which the Shell logs as
// "Can't update stage views actor ... needs an allocation".
export function meter(fraction) {
    const frac = Math.min(1, Math.max(0, fraction));
    const track = new St.BoxLayout({style_class: 'headroom-meter', x_expand: true});
    const fill = new St.Widget({style_class: 'headroom-meter-fill'});
    track.add_child(fill);
    let idle = 0;
    track.connect('notify::width', () => {
        if (idle)
            return;
        idle = GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
            idle = 0;
            fill.set_width(Math.round(track.width * frac));
            return GLib.SOURCE_REMOVE;
        });
    });
    track.connect('destroy', () => {
        if (idle)
            GLib.source_remove(idle);
        idle = 0;
    });
    return track;
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
