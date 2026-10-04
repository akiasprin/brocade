/** Shared bounds for the settings page and each machine's local log override. */
export const LOG_MIN_MIB = 10;
export const LOG_MAX_MIB = 4096;

export const validLogMib = (raw: string) => {
  if (!/^\d+$/.test(raw.trim())) return null;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value >= LOG_MIN_MIB && value <= LOG_MAX_MIB ? value : null;
};
