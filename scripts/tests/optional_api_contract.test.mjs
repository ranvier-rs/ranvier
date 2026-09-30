import assert from 'node:assert/strict';
import test from 'node:test';
import { validateObserveProducts, observeIdentityInputs } from '../optional_api_contract.mjs';

const product = () => ({ pkg: { name: 'ranvier-observe', publish: [] }, target: { kind: ['lib'] } });

test('exact unpublished library accepted', () => assert.doesNotThrow(() => validateObserveProducts([product()])));
test('empty or duplicate product scope rejected', () => {
  for (const products of [[], [product(), product()]]) assert.throws(() => validateObserveProducts(products));
});
test('published, renamed, or non-library adapter rejected', () => {
  for (const publish of [null, ['crates-io']]) {
    const item = product(); item.pkg.publish = publish;
    assert.throws(() => validateObserveProducts([item]));
  }
  const renamed = product(); renamed.pkg.name = 'other';
  assert.throws(() => validateObserveProducts([renamed]));
  const binary = product(); binary.target.kind = ['bin'];
  assert.throws(() => validateObserveProducts([binary]));
});
test('identity covers all adapter source, lock and generator modules without duplicates', () => {
  assert.equal(new Set(observeIdentityInputs).size, observeIdentityInputs.length);
  assert.ok(Object.isFrozen(observeIdentityInputs));
  for (const file of ['lib.rs', 'metrics.rs', 'redaction.rs']) {
    assert.ok(observeIdentityInputs.includes(`extensions/observe/src/${file}`));
  }
  for (const file of ['Cargo.lock', 'scripts/api_surface_inventory.mjs', 'scripts/optional_api_contract.mjs']) {
    assert.ok(observeIdentityInputs.includes(file));
  }
});
