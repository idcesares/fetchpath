import test from 'node:test';
import assert from 'node:assert/strict';
import { validateGraph, readyTasks, validateEvidence } from '../../tools/tasks.mjs';

const task = (id, status = 'todo', dependsOn = []) => ({ id, title: 'Outcome', status, dependsOn, files: ['tools'], evidence: status === 'done' ? ['README.md'] : [], acceptanceIds: ['A01'], acceptance: 'Behavior', verification: 'Test', owner: null, priority: 'P1' });
const graph = (...tasks) => ({ schemaVersion: 1, tasks });

test('only unblocked todo tasks are ready', () => {
  const data = graph(task('FP-001', 'done'), task('FP-002', 'todo', ['FP-001']), task('FP-003', 'todo', ['FP-002']));
  assert.deepEqual(validateGraph(data), []);
  assert.deepEqual(readyTasks(data).map(t => t.id), ['FP-002']);
});
test('cycles, unknown dependencies and duplicate IDs fail', () => {
  assert.match(validateGraph(graph(task('FP-001', 'todo', ['FP-002']), task('FP-002', 'todo', ['FP-001']))).join(), /cycle/);
  assert.match(validateGraph(graph(task('FP-001', 'todo', ['FP-099']))).join(), /missing dependency/);
  assert.match(validateGraph(graph(task('FP-001'), task('FP-001'))).join(), /Duplicate/);
});
test('completion and active ownership cannot bypass prerequisites', () => {
  assert.match(validateGraph(graph(task('FP-001'), task('FP-002', 'done', ['FP-001']))).join(), /unfinished dependency/);
  assert.match(validateGraph(graph(task('FP-001', 'in_progress'))).join(), /needs owner/);
  assert.match(validateGraph(graph({ ...task('FP-001', 'done'), evidence: [] })).join(), /needs evidence/);
});
test('missing and escaping evidence are rejected', async () => {
  const data = graph({ ...task('FP-001', 'done'), evidence: ['not-a-real-evidence-file', '../outside.md'] });
  assert.match((await validateEvidence(data)).join(), /missing evidence.*escapes repository/);
});
test('unfinished tasks need a priority and ready tasks are ordered by it', () => {
  assert.match(validateGraph(graph({ ...task('FP-001'), priority: undefined })).join(), /needs priority/);
  assert.match(validateGraph(graph({ ...task('FP-001'), priority: 'P9' })).join(), /needs priority/);
  assert.deepEqual(validateGraph(graph({ ...task('FP-001', 'done'), priority: undefined })), []);
  const data = graph({ ...task('FP-001'), priority: 'P3' }, { ...task('FP-002'), priority: 'P0' }, task('FP-003'));
  assert.deepEqual(readyTasks(data).map(t => t.id), ['FP-002', 'FP-003', 'FP-001']);
});
test('review marks are limited to strong and standard', () => {
  assert.match(validateGraph(graph({ ...task('FP-001'), review: 'maybe' })).join(), /invalid review/);
  assert.deepEqual(validateGraph(graph({ ...task('FP-001'), review: 'strong' })), []);
});
