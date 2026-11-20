// Run: node extension/breakdown_logic.test.mjs
import assert from 'node:assert/strict';
import {BreakdownOpen, barFraction, emptyText, headerText} from './breakdown_logic.js';

assert.equal(headerText({title: 'Today', figures: ['$1.50', '3K tokens']}), 'Today · $1.50 · 3K tokens');
assert.equal(headerText({title: 'Today'}), 'Today');
assert.equal(emptyText('Codex'), "Codex didn't report any data for this range.");
assert.equal(emptyText(''), 'No data was reported for this range.');
assert.equal(barFraction({fraction: 0.667}), 0.667);
assert.equal(barFraction({fraction: 3}), 1);
assert.equal(barFraction({fraction: -1}), 0);
assert.equal(barFraction({}), null);

const open = new BreakdownOpen();
const k = BreakdownOpen.key({card_id: 'a', title: 'Today'});
assert.equal(open.isOpen(k), false);
open.toggle(k);
assert.equal(open.isOpen(k), true);
assert.notEqual(k, BreakdownOpen.key({card_id: 'b', title: 'Today'}));
open.toggle(k);
assert.equal(open.isOpen(k), false);
console.log('ok');
