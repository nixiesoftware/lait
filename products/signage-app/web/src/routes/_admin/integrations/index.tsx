import { createFileRoute } from '@tanstack/react-router';
import IntegrationsListPage from '@/app/(admin)/(pages)/integrations/IntegrationsList';
import PageBreadCrumb from '@/components/common/PageBreadCrumb';

function IntegrationsRoute() {
  document.title = 'Integrations | AD2SP';
  return (
    <div className="space-y-4">
      <PageBreadCrumb pageTitle="Integrations" />
      <IntegrationsListPage />
    </div>
  );
}

export const Route = createFileRoute('/_admin/integrations/')({
  component: IntegrationsRoute,
});
