// Internal argument builder; release policy selects the cohort before this call.
export function packageCohortArgs(names) {
  if (!Array.isArray(names) || names.length === 0 || names.some(name => typeof name !== 'string' || !/^[a-z][a-z0-9_-]*$/.test(name)) || new Set(names).size !== names.length) {
    throw new Error('package cohort requires unique nonempty crate names');
  }
  return ['package', '--locked', '--no-verify', ...names.flatMap(name => ['-p', name])];
}
