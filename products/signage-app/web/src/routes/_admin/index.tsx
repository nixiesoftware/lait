import { createFileRoute } from '@tanstack/react-router';
import ScreenList from '@/app/(admin)/(pages)/screen-list/ScreenList';

function HomePage() {
  document.title = 'Screens | Signage';
  return <ScreenList />;
}

export const Route = createFileRoute('/_admin/')({
  component: HomePage,
});
