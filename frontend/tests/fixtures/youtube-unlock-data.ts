import type { UnlockResult } from '../../src/api';

export const youtubeSplitResult: UnlockResult = {
  service: 'youtube_premium', status: 'unsupported_region', code: 'region_unsupported', region: 'MO',
  youtube: {
    webpage: { status: 'available', code: 'web_available', http_status: 200, page_region: 'US', topbar_region: null, privacy_region: 'MO' },
    premium: { status: 'unsupported_region', code: 'region_unsupported', http_status: 200, page_region: 'US', topbar_region: null, privacy_region: 'MO' },
    cdn: { http_status: 200, node: 'hkg12s15', airport: 'HKG', country: 'HK', error: null, elapsed_ms: 1200 },
  },
};
