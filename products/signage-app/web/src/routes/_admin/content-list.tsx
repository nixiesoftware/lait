import { createFileRoute } from '@tanstack/react-router';
import { ContentListPage } from '@/app/(admin)/(pages)/content-list/ContentList';

interface ContentListSearch {
  q?: string;
}

function ContentListRoute() {
  document.title = 'Content | AD2SP';
  return <ContentListPage />;
}

export const Route = createFileRoute('/_admin/content-list')({
  component: ContentListRoute,
  validateSearch: (search: Record<string, unknown>): ContentListSearch => ({
    q: typeof search.q === 'string' ? search.q : undefined,
  }),
});
