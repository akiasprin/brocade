import type { InputHTMLAttributes } from 'react';

type ConfigSecretInputProps = Omit<
  InputHTMLAttributes<HTMLInputElement>,
  'type' | 'autoComplete' | 'autoCapitalize' | 'spellCheck'
>;

/**
 * A masked protocol/API secret that must not participate in browser login credential detection.
 *
 * Browsers may ignore autocomplete="off" on password inputs and pair a nearby configuration field
 * with the secret as a username. Keeping the native input as text avoids that credential heuristic;
 * the shared CSS class supplies the visual mask.
 */
export function ConfigSecretInput({ className, ...props }: ConfigSecretInputProps) {
  return (
    <input
      {...props}
      className={[className, 'config-secret-input'].filter(Boolean).join(' ')}
      type="text"
      autoComplete="off"
      autoCapitalize="none"
      spellCheck={false}
      data-1p-ignore
      data-lpignore="true"
      data-bwignore
    />
  );
}
