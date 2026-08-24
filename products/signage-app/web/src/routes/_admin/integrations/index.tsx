import { createFileRoute } from '@tanstack/react-router';
import IntegrationsListPage from '@/app/(admin)/(pages)/integrations/IntegrationsList';

function IntegrationsRoute() {
  document.title = 'Apps | Signage';
  return <IntegrationsListPage />;
}

export const Route = createFileRoute('/_admin/integrations/')({
  component: IntegrationsRoute,
});
