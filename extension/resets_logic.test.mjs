// Run: node extension/resets_logic.test.mjs
import assert from 'node:assert/strict';
import {ResetFlow, entryKeys, liveKeys, claimParameter} from './resets_logic.js';

let n = 0;
const flow = new ResetFlow(() => `id${++n}`);
const entries = [{claim: {claudeGrant: 'g'}}, {claim: {claudeGrant: 'g'}}];
const [a, b] = entryKeys('c', entries);
assert.notEqual(a, b);
assert.equal(flow.confirm(a), null);
flow.ask(a);
const id = flow.confirm(a);
assert.equal(id, 'id1');
flow.ask(b);
assert.equal(flow.confirm(b), null); // identical reset in flight under the same id
assert.equal(flow.finish(id, 'Couldn\'t connect', false), 'c');
assert.equal(flow.step(a), 'confirming');
assert.equal(flow.confirm(a), 'id1'); // retried under the same id
flow.finish('id1', 'Reset claimed. Enjoy!', true);
assert.equal(flow.step(a), null);
assert.equal(flow.message('c'), 'Reset claimed. Enjoy!');
flow.cancel(b);
flow.ask(a);
assert.equal(flow.confirm(a), 'id2'); // a settled claim's id is retired
flow.ask(b);
flow.reconcile(new Set());
assert.equal(flow.step(b), null);
assert.equal(flow.step(a), 'claiming'); // claims in flight survive a reload
assert.deepEqual([...liveKeys([{rows: [{card_id: 'c', resets: entries}, {title: 'x'}]}])], [a, b]);
flow.toggle('c');
assert.ok(flow.isOpen('c'));
flow.toggle('c');
assert.ok(!flow.isOpen('c'));
assert.deepEqual(JSON.parse(claimParameter(a, {claudeGrant: 'g'}, 'id2')), {card_id: 'c', claim: {claudeGrant: 'g'}, request_id: 'id2'});
const [u1, u2] = entryKeys('c', [{claim: {g: 1}, cardId: 'x'}, {claim: {g: 1}, cardId: 'y'}]);
assert.equal(JSON.parse(u1)[0], 'x');
assert.equal(JSON.parse(u2)[0], 'y'); // a unified row claims on the entry's own account
console.log('ok');
