import { createFileRoute } from '@tanstack/react-router';
import { ScreenDetail } from '@/app/(admin)/(pages)/screen-list/[id]/ScreenDetail';

function ScreenDetailPage() {
  const { id } = Route.useParams();
  document.title = 'Screen Details | AD2SP';
  return <ScreenDetail screenId={id} />;
}

export const Route = createFileRoute('/_admin/screen-list/$id')({
  component: ScreenDetailPage,
});
