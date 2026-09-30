// The optional adapter is a separate release surface, never an empty success.
export function validateObserveProducts(products) {
  const product = products[0];
  if (products.length !== 1 || product?.pkg.name !== 'ranvier-observe'
      || !Array.isArray(product.pkg.publish) || product.pkg.publish.length !== 0
      || !product.target?.kind.includes('lib')) {
    throw new Error('optional API inventory requires exactly one unpublished ranvier-observe library');
  }
}

export const observeIdentityInputs = Object.freeze([
  'Cargo.lock',
  'scripts/api_surface_inventory.mjs',
  'scripts/optional_api_contract.mjs',
  'extensions/observe/src/lib.rs',
  'extensions/observe/src/metrics.rs',
  'extensions/observe/src/redaction.rs'
]);
