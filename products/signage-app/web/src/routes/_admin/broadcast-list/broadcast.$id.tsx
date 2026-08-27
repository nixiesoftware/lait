import { createFileRoute } from '@tanstack/react-router';
import BroadcastDetailPage from '@/app/(admin)/(pages)/broadcast-list/broadcast/Broadcast';

function BroadcastDetailRoute() {
  document.title = "Program";
  return (
    <div className="h-full">
      <BroadcastDetailPage />
    </div>
  );
}

export const Route = createFileRoute('/_admin/broadcast-list/broadcast/$id')({
  component: BroadcastDetailRoute,
});
