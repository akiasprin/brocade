import { Children, Fragment, isValidElement, type ReactNode } from 'react';

function actionItems(children: ReactNode, parentKey = ''): Array<{ key: string; node: ReactNode }> {
  return Children.toArray(children).flatMap((child, index) => {
    const key = `${parentKey}/${isValidElement(child) ? (child.key ?? index) : index}`;
    return isValidElement<{ children?: ReactNode }>(child) && child.type === Fragment
      ? actionItems(child.props.children, key)
      : [{ key, node: child }];
  });
}

/** Page actions share one segmented surface; the principal action keeps its own emphasis. */
export function TitleActions({
  children,
  primary,
  status,
  className = '',
  label = '页面操作',
}: {
  children?: ReactNode;
  primary?: ReactNode;
  status?: ReactNode;
  className?: string;
  label?: string;
}) {
  const items = actionItems(children);
  return (
    <div className={`nd-tools title-actions ${className}`.trim()} role="group" aria-label={label}>
      {status}
      {items.length > 0 && (
        <div className="title-action-group">
          {items.map(item => (
            <div className="title-action-item" key={item.key}>
              {item.node}
            </div>
          ))}
        </div>
      )}
      {primary && <div className="title-action-primary">{primary}</div>}
    </div>
  );
}
