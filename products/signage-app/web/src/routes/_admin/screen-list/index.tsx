import { createFileRoute } from '@tanstack/react-router';
import { ScreenList } from '@/app/(admin)/(pages)/screen-list/ScreenList';

interface ScreenListSearch {
  q?: string;
}

function ScreenListPage() {
  document.title = 'Screens | AD2SP';
  return <ScreenList />;
}

export const Route = createFileRoute('/_admin/screen-list/')({
  component: ScreenListPage,
  validateSearch: (search: Record<string, unknown>): ScreenListSearch => ({
    q: typeof search.q === 'string' ? search.q : undefined,
  }),
});
