/**
 * React hooks over the create-and-navigate actions.
 */

import { useState, useCallback } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { createAndNavigateToBroadcast } from './actions';
import { createScreen } from '@/utils/screens/api';
import type { SignageProgram, SignageScreen } from '../lait/types';

export function useCreateBroadcast(onSuccess?: () => void) {
  const navigate = useNavigate();
  const [isCreating, setIsCreating] = useState(false);
  const [error, setError] = useState<Error | null>(null);

  const handleCreate = useCallback(
    async (name?: string): Promise<SignageProgram | null> => {
      if (isCreating) return null;
      setIsCreating(true);
      setError(null);
      const result = await createAndNavigateToBroadcast(name, {
        router: { push: (url: string) => navigate({ to: url }) },
        onSuccess,
        onError: (err) => {
          setError(err as Error);
          setIsCreating(false);
          return false;
        },
      });
      if (result) {
        setIsCreating(false);
      }
      return result;
    },
    [isCreating, navigate, onSuccess],
  );

  return { handleCreate, isCreating, error };
}

export function useCreateScreen(onSuccess?: () => void) {
  const [isCreating, setIsCreating] = useState(false);
  const [error, setError] = useState<Error | null>(null);

  const handleCreate = useCallback(
    async (name?: string): Promise<SignageScreen | null> => {
      if (isCreating) return null;
      setIsCreating(true);
      setError(null);
      try {
        const result = await createScreen(name ?? 'Unnamed Screen');
        setIsCreating(false);
        onSuccess?.();
        return result;
      } catch (err) {
        setError(err as Error);
        setIsCreating(false);
        return null;
      }
    },
    [isCreating, onSuccess],
  );

  return { handleCreate, isCreating, error };
}
