import { createFileRoute } from '@tanstack/react-router';
import ChannelList from '@/app/(admin)/(pages)/channel-list/ChannelList';

function ChannelListRoute() {
  document.title = 'Channels | Signage';
  return <ChannelList />;
}

export const Route = createFileRoute('/_admin/channel-list/')({
  component: ChannelListRoute,
});
