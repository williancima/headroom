// Run: node extension/layout_logic.test.mjs
import assert from 'node:assert/strict';
import {roles, menuIds, metricLabel, dropAction, providerIconFile, tooltipPosition} from './layout_logic.js';

const group = {kind: 'claude', unified: false, rows: [
    {title: 'Claude · a', card_id: 'c1'},
    {title: 'Weekly', id: 'c1/Weekly', card_id: 'c1'},
    {title: 'Rate Limit Resets', id: 'c1/Rate Limit Resets', card_id: 'c1', resets: [{}]},
    {title: 'Today', id: 'c1/Today', card_id: 'c1', breakdown: {}},
    {title: 'Claude · b', card_id: 'c2'},
    {title: 'Session', id: 'c2/Session'},
    {title: 'Note'},
]};
assert.deepEqual(roles(group), [
    {type: 'header', cardId: 'c1'}, {type: 'metric', cardId: 'c1'}, {type: 'metric', cardId: 'c1'}, {type: 'metric', cardId: 'c1'},
    {type: 'header', cardId: 'c2'}, {type: 'metric', cardId: 'c2'}, null,
]);
assert.deepEqual(roles({unified: true, rows: [{id: 'c1/Weekly'}]}), [{type: 'metric', cardId: null}]);
assert.deepEqual(roles({rows: [{id: 'x', members: [{}]}]}), [{type: 'metric', cardId: null}]);

assert.deepEqual(menuIds({id: 'c1/Weekly'}), ['c1/Weekly']);
assert.deepEqual(menuIds({id: 'c1/Weekly', copy_ids: []}), ['c1/Weekly']);
assert.deepEqual(menuIds({id: 'c1/Weekly', copy_ids: ['c1/Weekly', 'c2/Weekly']}), ['c1/Weekly', 'c2/Weekly']);
assert.equal(menuIds({title: 'header', card_id: 'c1'}), null);
assert.equal(metricLabel('c1/Rate Limit Resets', 'c1'), 'Rate Limit Resets');

const m = (cardId, label) => ({type: 'metric', cardId, label});
assert.deepEqual(dropAction(m('c1', 'A'), m('c1', 'B')), ['move-metric', '(sss)', ['c1', 'A', 'B']]);
assert.equal(dropAction(m('c1', 'A'), m('c2', 'B')), null);
assert.deepEqual(dropAction({type: 'account', cardId: 'c1'}, {type: 'account', cardId: 'c2'}), ['move-account', '(ss)', ['c1', 'c2']]);
assert.deepEqual(dropAction({type: 'provider', kind: 'claude'}, {type: 'provider', kind: 'codex'}), ['move-provider', '(ss)', ['claude', 'codex']]);
assert.equal(dropAction({type: 'account', cardId: 'c1'}, {type: 'provider', kind: 'x'}), null);
assert.equal(providerIconFile('claude'), 'icons/headroom-provider-claude-symbolic.svg');
console.log('ok');

const monitor = {x: 0, y: 0, width: 1000, height: 800};
assert.deepEqual(tooltipPosition({x: 100, y: 300, width: 32, height: 32}, {width: 60, height: 20}, monitor), {x: 86, y: 338});
assert.deepEqual(tooltipPosition({x: 980, y: 300, width: 20, height: 20}, {width: 100, height: 20}, monitor), {x: 900, y: 326});
assert.deepEqual(tooltipPosition({x: 0, y: 300, width: 20, height: 20}, {width: 100, height: 20}, monitor), {x: 0, y: 326});
assert.deepEqual(tooltipPosition({x: 100, y: 770, width: 32, height: 24}, {width: 60, height: 20}, monitor), {x: 86, y: 744});
