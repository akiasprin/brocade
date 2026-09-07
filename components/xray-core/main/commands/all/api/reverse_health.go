package api

import (
	"fmt"
	statsService "github.com/xtls/xray-core/app/stats/command"
	"github.com/xtls/xray-core/main/commands/base"
)

var cmdReverseHealth = &base.Command{
	CustomFlags: true,
	UsageLine:   "{{.Exec}} api reversehealth [--server=127.0.0.1:10085] [-watch]",
	Short:       "Read reverse tunnel health and business canaries",
	Long:        "Read a reverse health JSON snapshot, or watch snapshots and transitions. Use -timeout to bound a watch.",
	Run: func(cmd *base.Command, args []string) {
		setSharedFlags(cmd)
		watch := cmd.Flag.Bool("watch", false, "watch health changes")
		cmd.Flag.Parse(args)
		conn, ctx, close := dialAPIServer()
		defer close()
		client := statsService.NewStatsServiceClient(conn)
		request := &statsService.ReverseHealthRequest{}
		if *watch {
			stream, err := client.WatchReverseHealth(ctx, request)
			if err != nil {
				base.Fatalf("reverse health watch failed: %s", err)
			}
			for {
				response, err := stream.Recv()
				if err != nil {
					if ctx.Err() != nil {
						return
					}
					base.Fatalf("reverse health watch failed: %s", err)
				}
				fmt.Println(string(response.Json))
			}
		}
		response, err := client.GetReverseHealthSnapshot(ctx, request)
		if err != nil {
			base.Fatalf("reverse health snapshot failed: %s", err)
		}
		fmt.Println(string(response.Json))
	},
}
