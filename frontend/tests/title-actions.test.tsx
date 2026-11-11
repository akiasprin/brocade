import { Fragment, useState } from 'react';
import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { TitleActions } from '../src/ui/title-actions';

afterEach(cleanup);

function StatefulAction({ name }: { name: string }) {
  const [count, setCount] = useState(0);
  return (
    <button onClick={() => setCount(value => value + 1)}>
      {name} {count}
    </button>
  );
}

function Actions({ editing }: { editing: boolean }) {
  return (
    <TitleActions>
      <Fragment key="editing">{editing && <StatefulAction key="action" name="编辑" />}</Fragment>
      <Fragment key="persistent">
        <StatefulAction key="action" name="更多" />
      </Fragment>
    </TitleActions>
  );
}

it('条件操作消失时保留其他分组操作的状态，片段内的相同 key 不冲突', () => {
  const errors = vi.spyOn(console, 'error');
  const view = render(<Actions editing />);
  fireEvent.click(view.getByRole('button', { name: '更多 0' }));
  view.rerender(<Actions editing={false} />);
  expect(view.getByRole('button', { name: '更多 1' })).toBeTruthy();
  expect(view.queryByRole('button', { name: /编辑/ })).toBeNull();
  expect(errors).not.toHaveBeenCalled();
});
