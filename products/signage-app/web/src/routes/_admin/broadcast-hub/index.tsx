import { createFileRoute } from '@tanstack/react-router';
import BroadcastHub from '@/app/(admin)/(pages)/broadcast-hub/BroadcastHub';

/** A screen's page can hand one screen to the composer, pre-addressed. */
type Search = { screen?: string };

function BroadcastHubRoute() {
  const { screen } = Route.useSearch();
  document.title = 'Broadcasts | Signage';
  return <BroadcastHub screen={screen} />;
}

export const Route = createFileRoute('/_admin/broadcast-hub/')({
  validateSearch: (search: Record<string, unknown>): Search => ({
    screen: typeof search.screen === 'string' && search.screen ? search.screen : undefined,
  }),
  component: BroadcastHubRoute,
});
