export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
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
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      /* 非 JSON 响应，保留状态行 */
    }
    throw new ApiError(res.status, message);
  }
  if (res.status === 204) return undefined as T;
  return res.json() as Promise<T>;
}
