import { Button } from "@planetiler-rs/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@planetiler-rs/ui/card";

function GateShell({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex min-h-dvh items-center justify-center px-4">
      <Card className="w-full max-w-sm">{children}</Card>
    </div>
  );
}

export function SignInGate({ onSignIn }: { onSignIn: () => void }) {
  return (
    <GateShell>
      <CardHeader>
        <CardTitle>runner</CardTitle>
        <CardDescription>
          Console for planetiler-rs cloud runs. Sign in with the AWS access portal to continue.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <Button variant="solid" size="md" className="w-full" onClick={onSignIn}>
          Sign in
        </Button>
      </CardContent>
    </GateShell>
  );
}

export function AuthErrorGate({ message }: { message: string }) {
  return (
    <GateShell>
      <CardHeader>
        <CardTitle>runner</CardTitle>
        <CardDescription>{message}</CardDescription>
      </CardHeader>
      <CardContent>
        <Button variant="outline" size="md" className="w-full" onClick={() => location.assign("/")}>
          Try again
        </Button>
      </CardContent>
    </GateShell>
  );
}
