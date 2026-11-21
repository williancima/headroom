import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {expectedSharePath, shareRequest} from './share_logic.js';

const APP_NAME = 'dev.headroom.Headroom';
const APP_PATH = '/dev/headroom/Headroom';
const PENDING_SECONDS = 30;

// Asks the app for a share image and puts it on the clipboard (Wayland: only the shell may set it).
export class Share {
    // `command(name, params)` calls an app action; `fail(message)` shows an error.
    constructor(command, fail) {
        this._command = command;
        this._fail = fail;
        this._pending = new Map(); // requestId -> timeout source
        this._cancellable = new Gio.Cancellable();
        this._sub = Gio.DBus.session.signal_subscribe(APP_NAME, 'dev.headroom.Headroom.Share', 'ShareReady', APP_PATH, null,
            Gio.DBusSignalFlags.NONE, (_c, _s, _p, _i, _n, params) => this._ready(...params.deepUnpack()));
    }

    request(kind, rowId) {
        const requestId = GLib.uuid_string_random();
        this._pending.set(requestId, GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, PENDING_SECONDS, () => {
            this._pending.delete(requestId);
            return GLib.SOURCE_REMOVE;
        }));
        this._command('share-image', [GLib.Variant.new_string(shareRequest(kind, rowId, requestId))], () => this._drop(requestId));
    }

    _drop(requestId) {
        const timer = this._pending.get(requestId);
        if (timer === undefined)
            return false;
        GLib.source_remove(timer);
        this._pending.delete(requestId);
        return true;
    }

    _ready(requestId, path, error) {
        if (!this._drop(requestId))
            return;
        if (error) {
            this._fail(error);
            return;
        }
        if (path !== expectedSharePath(GLib.get_user_runtime_dir(), requestId)) {
            this._fail('Could not copy the image: the app sent an unexpected file.');
            return;
        }
        const file = Gio.File.new_for_path(path);
        file.load_bytes_async(this._cancellable, (f, res) => {
            try {
                const [bytes] = f.load_bytes_finish(res);
                St.Clipboard.get_default().set_content(St.ClipboardType.CLIPBOARD, 'image/png', bytes);
                Main.notify('Headroom', 'Copied to Clipboard');
            } catch (e) {
                if (!e.matches?.(Gio.IOErrorEnum, Gio.IOErrorEnum.CANCELLED))
                    this._fail(`Could not copy the image: ${e.message}`);
            }
            f.delete_async(GLib.PRIORITY_DEFAULT, null, (g, r) => {
                try {
                    g.delete_finish(r);
                } catch (_e) {
                    // Already gone; nothing to clean up.
                }
            });
        });
    }

    destroy() {
        this._cancellable.cancel();
        Gio.DBus.session.signal_unsubscribe(this._sub);
        for (const timer of this._pending.values())
            GLib.source_remove(timer);
        this._pending.clear();
    }
}
