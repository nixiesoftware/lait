import { createFileRoute } from '@tanstack/react-router';
import { BroadcastListPage } from '@/app/(admin)/(pages)/broadcast-list/BroadcastList';

interface BroadcastListSearch {
  q?: string;
}

function BroadcastListRoute() {
  document.title = 'Broadcasts | AD2SP';
  return <BroadcastListPage />;
}

export const Route = createFileRoute('/_admin/broadcast-list/')({
  component: BroadcastListRoute,
  validateSearch: (search: Record<string, unknown>): BroadcastListSearch => ({
    q: typeof search.q === 'string' ? search.q : undefined,
  }),
});
