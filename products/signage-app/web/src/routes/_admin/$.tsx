import { createFileRoute, Link } from '@tanstack/react-router';
import { Empty, Page } from '@/ds';

function NotFoundPage() {
  document.title = 'Not found — Signage';
  return (
    <Page>
      <Empty title="There is nothing at this address.">
        <Link to="/" className="ds-btn ds-btn-solid">
          Back to Screens
        </Link>
      </Empty>
    </Page>
  );
}

export const Route = createFileRoute('/_admin/$')({
  component: NotFoundPage,
});
