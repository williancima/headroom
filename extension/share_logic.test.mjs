import assert from 'node:assert/strict';
import {expectedSharePath, priorityCopyVisible, shareRequest} from './share_logic.js';

assert.equal(priorityCopyVisible('priority', [{rows: [{}]}]), true);
assert.equal(priorityCopyVisible('usage', [{rows: [{}]}]), false);
assert.equal(priorityCopyVisible('priority', [{rows: []}, {}]), false);
assert.equal(priorityCopyVisible('priority', undefined), false);

assert.deepEqual(JSON.parse(shareRequest('priority', 'x', 'abc-1')), {kind: 'priority', requestId: 'abc-1'});
assert.deepEqual(JSON.parse(shareRequest('breakdown', 'or@1/This Month', 'abc-2')),
    {kind: 'breakdown', rowId: 'or@1/This Month', requestId: 'abc-2'});
assert.equal(expectedSharePath('/run/user/1000', 'abc-1'), '/run/user/1000/headroom/share-abc-1.png');
assert.equal(expectedSharePath('/run/user/1000/', 'abc-1'), '/run/user/1000/headroom/share-abc-1.png');
console.log('share_logic ok');
