// The optional adapter is a separate release surface, never an empty success.
export function validateObserveProducts(products) {
  if (products.length !== 1 || products[0].pkg.name !== 'ranvier-observe') {
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
