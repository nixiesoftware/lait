import React, { createContext, useContext, useState, ReactNode } from "react";

interface AdminLayoutContextType {
  hideSidebar: boolean;
  setHideSidebar: (hide: boolean) => void;
}

const AdminLayoutContext = createContext<AdminLayoutContextType | undefined>(undefined);

export const useAdminLayout = () => {
  const context = useContext(AdminLayoutContext);
  if (!context) {
    throw new Error("useAdminLayout must be used within AdminLayoutProvider");
  }
  return context;
};

interface AdminLayoutProviderProps {
  children: ReactNode;
}

export const AdminLayoutProvider: React.FC<AdminLayoutProviderProps> = ({ children }) => {
  const [hideSidebar, setHideSidebar] = useState(false);

  return (
    <AdminLayoutContext.Provider
      value={{
        hideSidebar,
        setHideSidebar
      }}
    >
      {children}
    </AdminLayoutContext.Provider>
  );
};