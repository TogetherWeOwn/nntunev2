import { PUBLIC_API_BASE_URL } from '$app/env/public';
import { ApiClient, ApiError } from '../api/client.js';
import { validSessionBinding as validBinding, type AccountSession } from '../auth-fence.js';

export { validBinding };

export async function verifyLibrarySession(binding: string, signal: AbortSignal, owner: AccountSession): Promise<boolean> {
	try {
		const result = await new ApiClient({ baseUrl: PUBLIC_API_BASE_URL, signal, session: { owner } })
			.get<{ session_binding?: unknown }>('/v1/me/likes/session');
		if (!validBinding(result?.session_binding)) throw new Error('Missing verified session binding');
		return result.session_binding === binding;
	} catch (error) {
		if (error instanceof ApiError && (error.status === 401 || error.status === 403)) return false;
		throw error;
	}
}
