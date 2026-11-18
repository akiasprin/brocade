export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
    public issues: import('./geodata').RuleIssue[] = [],
  ) {
    super(message);
  }
}

export async function api<T>(path: string, token = '', init?: RequestInit): Promise<T> {
  const headers: Record<string, string> = {
    ...(init?.body ? { 'content-type': 'application/json' } : {}),
    ...((init?.headers as Record<string, string> | undefined) ?? {}),
  };
  const bearer = token.trim();
  if (bearer) headers.authorization = `Bearer ${bearer}`;

  const res = await fetch(path, {
    ...init,
    credentials: 'same-origin',
    headers,
  });
  if (!res.ok) {
    /* 错误响应体格式为 {"error": "..."}（http.rs 的 ErrorBody），解析失败时回退到状态行 */
    let message = `${res.status} ${res.statusText}`;
    let issues: import('./geodata').RuleIssue[] = [];
    try {
      const body = (await res.json()) as { error?: string; issues?: import('./geodata').RuleIssue[] };
      if (body.error) message = body.error;
      if (Array.isArray(body.issues)) issues = body.issues;
    } catch {
      /* 非 JSON 响应，保留状态行 */
    }
    throw new ApiError(res.status, message, issues);
  }
  if (res.status === 204) return undefined as T;
  return res.json() as Promise<T>;
}
