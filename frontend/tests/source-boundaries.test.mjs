import assert from 'node:assert/strict';
import { readdirSync, readFileSync, realpathSync } from 'node:fs';
import { dirname, extname, isAbsolute, join, relative, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test from 'node:test';

const frontend = fileURLToPath(new URL('..', import.meta.url));
const root = resolve(frontend, '..');
const localPrototypes = join(root, 'mockups');
const sources = new Set(['.ts', '.tsx', '.mjs', '.js', '.html', '.css']);
const walk = path =>
  readdirSync(path, { withFileTypes: true }).flatMap(entry => {
    const file = join(path, entry.name);
    return entry.isDirectory() ? walk(file) : [file];
  });
const files = ['src', 'tests', 'scripts'].flatMap(path => walk(join(frontend, path)));
const insidePrototypes = path => path === localPrototypes || path.startsWith(localPrototypes + '/');

function pathReferencesPrototype(file, path) {
  if (path.startsWith('/@fs/')) return insidePrototypes(resolve(path.slice('/@fs'.length)));
  if (isAbsolute(path)) {
    // Browser root URLs and filesystem paths have different roots.
    return insidePrototypes(resolve(path)) || insidePrototypes(resolve(root, '.' + path));
  }
  return path.startsWith('.') && insidePrototypes(resolve(dirname(file), path));
}

function referencesPrototype(file, reference) {
  const path = reference.split(/[?#]/, 1)[0];
  if (path.startsWith('file:')) {
    try {
      return insidePrototypes(fileURLToPath(path));
    } catch {
      // Quoted malformed-URL examples are data, rather than filesystem dependencies.
      return false;
    }
  }
  if (!path.startsWith('.') && !path.startsWith('/')) return false;
  if (pathReferencesPrototype(file, path)) return true;
  try {
    return pathReferencesPrototype(file, decodeURIComponent(path));
  } catch {
    return false;
  }
}

test('prototype boundary recognizes relative paths, filesystem URLs and Vite URLs', () => {
  const file = join(frontend, 'tests', 'fixtures', 'entry.ts');
  const prototype = join(localPrototypes, 'boundary-example.ts');
  for (const reference of [
    relative(dirname(file), prototype),
    '/' + relative(root, prototype),
    prototype,
    '/@fs' + prototype,
    pathToFileURL(prototype).href,
    '/@fs' + prototype.replace('mockups', '%6dockups'),
    prototype + '%',
  ]) {
    assert.equal(referencesPrototype(file, reference), true, reference);
  }
  for (const reference of ['../../src/api', '/src/main.tsx', '@tanstack/react-query', '%', '/invalid-%']) {
    assert.equal(referencesPrototype(file, reference), false, reference);
  }
});

test('frontend source and regression tests contain no design prototype assets', () => {
  assert.ok(files.length > 0, 'inspect the actual frontend tree');
  const prototypes = files.filter(
    file => /mockup/i.test(relative(frontend, file)) || insidePrototypes(realpathSync(file)),
  );
  assert.deepEqual(
    prototypes.map(file => relative(frontend, file)),
    [],
  );
});

test('frontend imports and browser fixtures cannot depend on local design prototypes', () => {
  const dependencies = [];
  for (const file of files.filter(file => sources.has(extname(file)))) {
    const text = readFileSync(file, 'utf8');
    // Includes module imports, dynamic imports, fixture URLs and HTML asset references.
    for (const [, reference] of text.matchAll(/["'`]([^"'`\n]+)["'`]/g)) {
      if (referencesPrototype(file, reference)) {
        dependencies.push({ file: relative(frontend, file), reference });
      }
    }
  }
  assert.deepEqual(dependencies, []);
});
