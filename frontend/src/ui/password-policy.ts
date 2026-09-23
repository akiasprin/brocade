export const PASSWORD_MIN_LENGTH = 8;

export function passwordConfirmation(password: string, confirmation: string) {
  return {
    tooShort: password.length > 0 && password.length < PASSWORD_MIN_LENGTH,
    mismatch: confirmation.length > 0 && password !== confirmation,
    ready: password.length >= PASSWORD_MIN_LENGTH && password === confirmation,
  };
}
