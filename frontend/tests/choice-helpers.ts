import { fireEvent, screen } from '@testing-library/react';

/** Select a visible custom option, or type into an ordinary form control. */
export function changeControl(element: Element, event: { target: { value: string | number } }) {
  if (!(element instanceof HTMLButtonElement) || element.getAttribute('role') !== 'combobox') {
    fireEvent.change(element, event);
    return;
  }
  fireEvent.click(element);
  const option = screen
    .getAllByRole('option')
    .find(option => option.getAttribute('data-value') === String(event.target.value));
  if (!option) throw new Error(`Choice has no option ${event.target.value}`);
  fireEvent.click(option);
}

export function choiceOptions(element: Element) {
  fireEvent.click(element);
  const options = screen.getAllByRole('option').map(option => ({
    value: option.getAttribute('data-value'),
    text: option.querySelector('.choice-menu-copy')?.textContent,
    disabled: (option as HTMLButtonElement).disabled,
  }));
  // Selecting the current value closes the popup without changing configuration.
  const selected = screen.getAllByRole('option').find(option => option.getAttribute('aria-selected') === 'true');
  if (!selected) throw new Error('Choice has no current option');
  fireEvent.click(selected);
  return options;
}
