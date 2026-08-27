import { createFileRoute } from '@tanstack/react-router';
import BroadcastHub from '@/app/(admin)/(pages)/broadcast-hub/BroadcastHub';

function BroadcastHubRoute() {
  document.title = 'Broadcasts | Signage';
  return <BroadcastHub />;
}

export const Route = createFileRoute('/_admin/broadcast-hub/')({
  component: BroadcastHubRoute,
});
