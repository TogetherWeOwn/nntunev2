import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { ApiClient, ApiError } from '../api/client.js';

// SHA-256 base64url has two zero pad bits in its last sextet.
export const validBinding = (value: unknown): value is string => typeof value === 'string' && /^[\w-]{42}[AEIMQUYcgkosw048]$/.test(value);

export async function verifyLibrarySession(binding: string, signal: AbortSignal): Promise<boolean> {
	try {
		const result = await new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal })
			.get<{ session_binding?: unknown }>('/v1/me/likes/session');
		if (!validBinding(result?.session_binding)) throw new Error('Missing verified session binding');
		return result.session_binding === binding;
	} catch (error) {
		if (error instanceof ApiError && (error.status === 401 || error.status === 403)) return false;
		throw error;
	}
}
