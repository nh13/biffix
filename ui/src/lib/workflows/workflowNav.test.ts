import { describe, expect, it } from 'vitest';
import { activeWorkflowTab, workflowTabHref, workflowTabs } from './workflowNav';

describe('workflow tabs', () => {
  it('gives every tab its own path and encodes the id', () => {
    expect(workflowTabHref('sha:hash', 'run')).toBe('/workflows/sha%3Ahash/run');
    expect(workflowTabHref('sha:hash', 'parameters')).toBe('/workflows/sha%3Ahash/parameters');
    expect(workflowTabHref('sha:hash', 'versions')).toBe('/workflows/sha%3Ahash/versions');
  });
  it('reads the tab from the path, with the version URL on Run', () => {
    expect(activeWorkflowTab('/workflows/sha%3Ahash')).toBe('run');
    expect(activeWorkflowTab('/workflows/sha%3Ahash/parameters')).toBe('parameters');
    expect(activeWorkflowTab('/workflows/sha%3Ahash/provenance')).toBe('provenance');
    expect(activeWorkflowTab('/workflows/x/versions/')).toBe('versions');
    expect(activeWorkflowTab('/workflows/x/other')).toBe('run');
    expect(activeWorkflowTab('/workflows/x/about')).toBe('about');
  });
  it('shows About only for a version with an About page', () => {
    expect(workflowTabs({ about_markdown: '# Hi' })).toEqual([
      'run',
      'about',
      'parameters',
      'versions',
      'provenance',
    ]);
    expect(workflowTabs({})).not.toContain('about');
    expect(workflowTabs({ about_markdown: '' })).not.toContain('about');
  });
});
