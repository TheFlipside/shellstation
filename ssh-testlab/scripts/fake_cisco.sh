#!/bin/sh
# fake_cisco.sh — Simulates a Cisco IOS CLI for testing keyword highlighting.
# Supports common "show" commands with realistic output containing typical
# highlight keywords: up/down, error, BGP states, routing protocols, ACL
# permit/deny, syslog severities, and more.
#
# Used as the login shell for the cisco-sim container so that an SSH
# connection drops straight into a fake IOS prompt.

HOSTNAME="Router1"
ENABLE_MODE=0

prompt() {
    if [ "$ENABLE_MODE" -eq 1 ]; then
        printf '%s# ' "$HOSTNAME"
    else
        printf '%s> ' "$HOSTNAME"
    fi
}

show_version() {
    cat <<'EOF'
Cisco IOS Software, C2900 Software (C2900-UNIVERSALK9-M), Version 15.7(3)M9
Technical Support: http://www.cisco.com/techsupport
Copyright (c) 1986-2024 by Cisco Systems, Inc.

ROM: System Bootstrap, Version 15.0(1r)M16

Router1 uptime is 47 days, 3 hours, 22 minutes
System returned to ROM by power-on
System image file is "flash:c2900-universalk9-mz.SPA.157-3.M9.bin"

Cisco CISCO2911/K9 (revision 1.0) with 2048000K/4096K bytes of memory.
Processor board ID FTX1840A0RC
3 Gigabit Ethernet interfaces
1 Serial interface
1 terminal line
DRAM configuration is 64 bits wide with parity disabled.
256K bytes of non-volatile configuration memory.

Configuration register is 0x2102
EOF
}

show_interfaces() {
    cat <<'EOF'
GigabitEthernet0/0 is up, line protocol is up
  Hardware is iGbE, address is 001e.f763.a800 (bia 001e.f763.a800)
  Description: WAN Uplink
  Internet address is 203.0.113.1/24
  MTU 1500 bytes, BW 1000000 Kbit/sec, DLY 10 usec,
     reliability 255/255, txload 1/255, rxload 1/255
  Encapsulation ARPA, loopback not set
  Full-duplex, 1000Mbps, media type is RJ45
  Input flow-control is off, output flow-control is off
  5 minute input rate 8432000 bits/sec, 1204 packets/sec
  5 minute output rate 5221000 bits/sec, 743 packets/sec
     892341 packets input, 571802624 bytes, 0 no buffer
     Received 12043 broadcasts (0 IP multicasts)
     0 runts, 0 giants, 0 throttles
     0 input errors, 0 CRC, 0 frame, 0 overrun, 0 ignored
     0 watchdog, 0 multicast, 0 pause input
     743221 packets output, 312110438 bytes, 0 underruns
     0 output errors, 0 collisions, 0 interface resets
     0 unknown protocol drops
     0 babbles, 0 late collision, 0 deferred
     0 lost carrier, 0 no carrier, 0 pause output
     0 output buffer failures, 0 output buffers swapped out
GigabitEthernet0/1 is up, line protocol is up
  Hardware is iGbE, address is 001e.f763.a801 (bia 001e.f763.a801)
  Description: LAN Segment A
  Internet address is 10.0.1.1/24
  MTU 1500 bytes, BW 1000000 Kbit/sec, DLY 10 usec,
     reliability 255/255, txload 1/255, rxload 1/255
  Encapsulation ARPA, loopback not set
  Full-duplex, 1000Mbps, media type is RJ45
     3421789 packets input, 2191545984 bytes, 0 no buffer
     0 runts, 0 giants, 0 throttles
     3 input errors, 2 CRC, 1 frame, 0 overrun, 0 ignored
     2980112 packets output, 1908271616 bytes, 0 underruns
     0 output errors, 0 collisions, 0 interface resets
GigabitEthernet0/2 is administratively down, line protocol is down
  Hardware is iGbE, address is 001e.f763.a802 (bia 001e.f763.a802)
  Description: UNUSED
  MTU 1500 bytes, BW 1000000 Kbit/sec, DLY 10 usec,
     reliability 255/255, txload 0/255, rxload 0/255
  Encapsulation ARPA, loopback not set
     0 packets input, 0 bytes, 0 no buffer
     0 input errors, 0 CRC, 0 frame, 0 overrun, 0 ignored
     0 packets output, 0 bytes, 0 underruns
     0 output errors, 0 collisions, 0 interface resets
Serial0/0/0 is down, line protocol is down
  Hardware is WIC-1T
  Description: Backup WAN Link
  Internet address is 198.51.100.1/30
  MTU 1500 bytes, BW 1544 Kbit/sec, DLY 20000 usec,
     reliability 255/255, txload 0/255, rxload 0/255
  Encapsulation HDLC, loopback not set
     0 packets input, 0 bytes, 0 no buffer
     14 input errors, 7 CRC, 3 frame, 0 overrun, 4 abort
     0 packets output, 0 bytes, 0 underruns
     8 output errors, 0 collisions, 12 interface resets
     0 unknown protocol drops
     0 carrier transitions     DCD=down  DSR=down  DTR=down  RTS=up  CTS=down
Loopback0 is up, line protocol is up
  Hardware is Loopback
  Internet address is 10.255.255.1/32
  MTU 1514 bytes, BW 8000000 Kbit/sec, DLY 5000 usec,
     reliability 255/255, txload 0/255, rxload 0/255
     0 packets input, 0 bytes
     0 packets output, 0 bytes
EOF
}

show_ip_interface_brief() {
    cat <<'EOF'
Interface                  IP-Address      OK? Method Status                Protocol
GigabitEthernet0/0         203.0.113.1     YES manual up                    up
GigabitEthernet0/1         10.0.1.1        YES manual up                    up
GigabitEthernet0/2         unassigned      YES unset  administratively down down
Serial0/0/0                198.51.100.1    YES manual down                  down
Loopback0                  10.255.255.1    YES manual up                    up
EOF
}

show_ip_route() {
    cat <<'EOF'
Codes: L - local, C - connected, S - static, R - RIP, M - mobile, B - BGP
       D - EIGRP, EX - EIGRP external, O - OSPF, IA - OSPF inter area
       N1 - OSPF NSSA external type 1, N2 - OSPF NSSA external type 2
       E1 - OSPF external type 1, E2 - OSPF external type 2
       i - IS-IS, su - IS-IS summary, L1 - IS-IS level-1, L2 - IS-IS level-2
       ia - IS-IS inter area, * - candidate default, U - per-user static route
       o - ODR, P - periodic downloaded static route, H - NHRP, l - LISP
       a - application route
       + - replicated route, % - next hop override, p - overrides from PfR

Gateway of last resort is 203.0.113.254 to network 0.0.0.0

S*    0.0.0.0/0 [1/0] via 203.0.113.254
      10.0.0.0/8 is variably subnetted, 8 subnets, 3 masks
C        10.0.1.0/24 is directly connected, GigabitEthernet0/1
L        10.0.1.1/32 is directly connected, GigabitEthernet0/1
O        10.0.2.0/24 [110/20] via 10.0.1.2, 2d03h, GigabitEthernet0/1
O IA     10.0.3.0/24 [110/30] via 10.0.1.2, 1d12h, GigabitEthernet0/1
B        10.10.0.0/16 [20/0] via 203.0.113.2, 5d08h
D        10.20.0.0/16 [90/2570240] via 10.0.1.3, 3d01h, GigabitEthernet0/1
O E2     10.30.0.0/16 [110/20] via 10.0.1.2, 1d12h, GigabitEthernet0/1
S        10.40.0.0/16 [1/0] via 10.0.1.254
C        10.255.255.1/32 is directly connected, Loopback0
      172.16.0.0/16 is variably subnetted, 2 subnets, 2 masks
B        172.16.0.0/16 [20/100] via 203.0.113.2, 5d08h
O        172.16.10.0/24 [110/40] via 10.0.1.2, 2d03h, GigabitEthernet0/1
      192.168.0.0/24 is subnetted, 3 subnets
D        192.168.1.0 [90/2570240] via 10.0.1.3, 3d01h, GigabitEthernet0/1
O N2     192.168.2.0 [110/20] via 10.0.1.2, 1d12h, GigabitEthernet0/1
S        192.168.3.0 [1/0] via 10.0.1.254
      203.0.113.0/24 is variably subnetted, 2 subnets, 2 masks
C        203.0.113.0/24 is directly connected, GigabitEthernet0/0
L        203.0.113.1/32 is directly connected, GigabitEthernet0/0
EOF
}

show_ip_bgp_summary() {
    cat <<'EOF'
BGP router identifier 10.255.255.1, local AS number 65001
BGP table version is 142, main routing table version 142
18 network entries using 4464 bytes of memory
21 path entries using 2856 bytes of memory
6/4 BGP path/bestpath attribute entries using 1632 bytes of memory
3 BGP AS-PATH entries using 72 bytes of memory
0 BGP route-map cache entries using 0 bytes of memory
0 BGP filter-list cache entries using 0 bytes of memory
BGP using 9024 total bytes of memory
BGP activity 34/16 prefixes, 48/27 paths, scan interval 60 secs
18 networks peaked at 12:34:56 Apr 1 2026

Neighbor        V           AS MsgRcvd MsgSent   TblVer  InQ OutQ Up/Down  State/PfxRcd
203.0.113.2     4        65002   84321   72104      142    0    0 5d08h          12
203.0.113.3     4        65003   12044   11892      142    0    0 2d01h           6
203.0.113.4     4        65004       0       0        1    0    0 never    Active
203.0.113.5     4        65005     103      98        1    0    0 00:03:21 Idle
203.0.113.6     4        65006      42      40        1    0    0 00:00:45 Connect
203.0.113.7     4        65007      15      14        1    0    0 00:01:12 OpenSent
203.0.113.8     4        65008      28      27        1    0    0 00:00:08 OpenConfirm
10.0.1.10       4        65001   91003   90887      142    0    0 12d04h          8
EOF
}

show_ip_bgp() {
    cat <<'EOF'
BGP table version is 142, local router ID is 10.255.255.1
Status codes: s suppressed, d damped, h history, * valid, > best, i - internal,
              r RIB-failure, S Stale, m multipath, b backup-path, f RT-Filter,
              x best-external, a additional-path, c RIB-compressed,
              t secondary path, L long-lived-stale,
Origin codes: i - IGP, e - EGP, ? - incomplete
RPKI validation codes: V valid, I invalid, N Not found

     Network          Next Hop            Metric LocPrf Weight Path
 *>   10.10.0.0/16     203.0.113.2              0             0 65002 i
 *    10.10.0.0/16     203.0.113.3            100             0 65003 65002 i
 *>   10.10.1.0/24     203.0.113.2              0             0 65002 i
 *>i  172.16.0.0/16    10.0.1.10              100    200      0 65009 i
 *>   172.16.10.0/24   203.0.113.2            200             0 65002 65010 i
 s    192.168.50.0/24  203.0.113.4                            0 65004 i
 d    192.168.51.0/24  203.0.113.3            300             0 65003 i
 *>   0.0.0.0/0        203.0.113.2              0             0 65002 i
EOF
}

show_ip_ospf_neighbor() {
    cat <<'EOF'
Neighbor ID     Pri   State           Dead Time   Address         Interface
10.0.1.2          1   FULL/DR         00:00:33    10.0.1.2        GigabitEthernet0/1
10.0.1.5          1   FULL/BDR        00:00:31    10.0.1.5        GigabitEthernet0/1
10.0.1.6          1   2WAY/DROTHER    00:00:37    10.0.1.6        GigabitEthernet0/1
10.0.1.7          1   INIT/  -        00:00:38    10.0.1.7        GigabitEthernet0/1
10.0.1.8          0   DOWN/  -        00:00:40    10.0.1.8        GigabitEthernet0/1
10.0.1.9          1   EXSTART/  -     00:00:35    10.0.1.9        GigabitEthernet0/1
10.0.1.11         1   EXCHANGE/  -    00:00:36    10.0.1.11       GigabitEthernet0/1
10.0.1.12         1   LOADING/  -     00:00:34    10.0.1.12       GigabitEthernet0/1
EOF
}

show_access_lists() {
    cat <<'EOF'
Standard IP access list 10
    10 permit 10.0.1.0, wildcard bits 0.0.0.255 (2847 matches)
    20 permit 10.0.2.0, wildcard bits 0.0.0.255 (1203 matches)
    30 deny   any (47 matches)
Extended IP access list 101
    10 permit tcp 10.0.1.0 0.0.0.255 any eq 22 (12043 matches)
    20 permit tcp 10.0.1.0 0.0.0.255 any eq 443 (89412 matches)
    30 permit tcp 10.0.1.0 0.0.0.255 any eq 80 (34201 matches)
    40 permit icmp any any echo (5621 matches)
    50 permit icmp any any echo-reply (5590 matches)
    60 deny   tcp any any eq 23 log (312 matches)
    70 deny   tcp any any eq 3389 log (87 matches)
    80 permit ip 10.0.0.0 0.0.255.255 any (234001 matches)
    90 deny   ip any any log (1043 matches)
Extended IP access list OUTSIDE_IN
    10 permit tcp any host 203.0.113.10 eq 443 (443021 matches)
    20 permit tcp any host 203.0.113.10 eq 80 (221044 matches)
    30 permit tcp any host 203.0.113.11 eq 25 (12044 matches)
    40 deny   ip any any log (98721 matches)
EOF
}

show_logging() {
    cat <<'EOF'
Syslog logging: enabled (0 messages dropped, 0 messages rate-limited,
                0 flushes, 0 overflows, xml disabled, filtering disabled)

No Active Message Discriminator.

    Console logging: level debugging, 4821 messages logged, xml disabled,
                     filtering disabled
    Monitor logging: level debugging, 0 messages logged, xml disabled,
                     filtering disabled
    Buffer logging:  level debugging, 4821 messages logged, xml disabled,
                     filtering disabled
    Logging Exception size (4096 bytes)
    Count and timestamp logging messages: disabled
    Persistent logging: disabled

Log Buffer (65536 bytes):

*Apr  9 08:12:01.234: %LINK-3-UPDOWN: Interface GigabitEthernet0/2, changed state to down
*Apr  9 08:12:01.234: %LINEPROTO-5-UPDOWN: Line protocol on Interface GigabitEthernet0/2, changed state to down
*Apr  9 08:15:33.891: %SYS-5-CONFIG_I: Configured from console by admin on vty0 (10.0.1.100)
*Apr  9 08:22:14.112: %OSPF-5-ADJCHG: Process 1, Nbr 10.0.1.2 on GigabitEthernet0/1 from LOADING to FULL, Loading Done
*Apr  9 08:30:00.001: %SYS-6-LOGGINGHOST_STARTSTOP: Logging to host 10.0.1.200 port 514 started - CLI initiated
*Apr  9 09:01:44.567: %BGP-5-ADJCHANGE: neighbor 203.0.113.4 Down BGP Notification sent
*Apr  9 09:01:44.567: %BGP-3-NOTIFICATION: sent to neighbor 203.0.113.4 4/0 (hold time expired) 0 bytes
*Apr  9 09:05:12.890: %SEC-6-IPACCESSLOGP: list 101 denied tcp 192.168.1.50(44231) -> 203.0.113.1(23), 1 packet
*Apr  9 09:10:33.445: %DUAL-5-NBRCHANGE: EIGRP-IPv4 100: Neighbor 10.0.1.3 (GigabitEthernet0/1) is up: new adjacency
*Apr  9 09:15:21.678: %LINK-3-UPDOWN: Interface Serial0/0/0, changed state to down
*Apr  9 09:15:22.678: %LINEPROTO-5-UPDOWN: Line protocol on Interface Serial0/0/0, changed state to down
*Apr  9 09:45:00.123: %SYS-2-MALLOCFAIL: Memory allocation of 65536 bytes failed from 0x60A1B2C3, alignment 0, pool Processor
*Apr  9 10:00:01.001: %SNMP-3-AUTHFAIL: Authentication failure for SNMP req from host 192.168.1.99
*Apr  9 10:12:33.789: %TRACKING-5-STATE: 1 ip sla 1 reachability Up -> Down
*Apr  9 10:30:00.456: %SYS-1-CPURISINGTHRESHOLD: Threshold: Total CPU Utilization(sobrecarga) 95%
*Apr  9 10:30:05.789: %SYS-3-CPUHOG: Task ran for 2040 msec (4/0), process = IP Input, PC = 60B2C3D4
*Apr  9 10:45:12.234: %HSRP-5-STATECHANGE: GigabitEthernet0/1 Grp 1 state Standby -> Active
*Apr  9 11:00:00.567: %BGP-5-ADJCHANGE: neighbor 203.0.113.5 Up
*Apr  9 11:05:33.890: %LINEPROTO-5-UPDOWN: Line protocol on Interface GigabitEthernet0/0, changed state to up
*Apr  9 11:20:44.123: %SEC-6-IPACCESSLOGP: list OUTSIDE_IN denied ip 198.51.100.50(0) -> 203.0.113.1(0), 3 packets
*Apr  9 11:30:00.001: %SYS-4-CONFIG_RESOLVE_FAILURE: Unable to resolve hostname for logging server
*Apr  9 11:45:55.678: %PLATFORM-2-PF_MEMORY_ERROR: Memory error detected and corrected
EOF
}

show_running_config() {
    cat <<'EOF'
Building configuration...

Current configuration : 3842 bytes
!
! Last configuration change at 08:15:33 UTC Apr 9 2026 by admin
!
version 15.7
service timestamps debug datetime msec
service timestamps log datetime msec
service password-encryption
!
hostname Router1
!
boot-start-marker
boot-end-marker
!
enable secret 9 $9$XXXXXXXXXXXX
!
no aaa new-model
!
ip cef
no ip domain lookup
ip domain name lab.local
!
interface Loopback0
 ip address 10.255.255.1 255.255.255.255
 ip ospf 1 area 0
!
interface GigabitEthernet0/0
 description WAN Uplink
 ip address 203.0.113.1 255.255.255.0
 ip access-group OUTSIDE_IN in
 ip nat outside
 duplex full
 speed 1000
 no shutdown
!
interface GigabitEthernet0/1
 description LAN Segment A
 ip address 10.0.1.1 255.255.255.0
 ip nat inside
 ip ospf 1 area 0
 duplex full
 speed 1000
 no shutdown
!
interface GigabitEthernet0/2
 description UNUSED
 no ip address
 shutdown
!
interface Serial0/0/0
 description Backup WAN Link
 ip address 198.51.100.1 255.255.255.252
 encapsulation hdlc
 shutdown
!
router ospf 1
 router-id 10.255.255.1
 passive-interface GigabitEthernet0/0
 network 10.0.1.0 0.0.0.255 area 0
 network 10.255.255.1 0.0.0.0 area 0
 default-information originate
!
router bgp 65001
 bgp router-id 10.255.255.1
 bgp log-neighbor-changes
 neighbor 203.0.113.2 remote-as 65002
 neighbor 203.0.113.3 remote-as 65003
 neighbor 203.0.113.4 remote-as 65004
 neighbor 203.0.113.5 remote-as 65005
 neighbor 10.0.1.10 remote-as 65001
 neighbor 10.0.1.10 update-source Loopback0
 !
 address-family ipv4 unicast
  neighbor 203.0.113.2 activate
  neighbor 203.0.113.3 activate
  neighbor 203.0.113.4 activate
  neighbor 203.0.113.5 activate
  neighbor 10.0.1.10 activate
  neighbor 10.0.1.10 next-hop-self
 exit-address-family
!
ip nat inside source list 10 interface GigabitEthernet0/0 overload
!
ip access-list standard 10
 permit 10.0.1.0 0.0.0.255
 permit 10.0.2.0 0.0.0.255
 deny   any
!
ip access-list extended 101
 permit tcp 10.0.1.0 0.0.0.255 any eq 22
 permit tcp 10.0.1.0 0.0.0.255 any eq 443
 permit tcp 10.0.1.0 0.0.0.255 any eq 80
 permit icmp any any echo
 permit icmp any any echo-reply
 deny   tcp any any eq telnet log
 deny   tcp any any eq 3389 log
 permit ip 10.0.0.0 0.0.255.255 any
 deny   ip any any log
!
ip access-list extended OUTSIDE_IN
 permit tcp any host 203.0.113.10 eq 443
 permit tcp any host 203.0.113.10 eq 80
 permit tcp any host 203.0.113.11 eq 25
 deny   ip any any log
!
ip route 0.0.0.0 0.0.0.0 203.0.113.254
ip route 10.40.0.0 255.255.0.0 10.0.1.254
ip route 192.168.3.0 255.255.255.0 10.0.1.254
!
logging buffered 65536 debugging
logging host 10.0.1.200
!
line con 0
 exec-timeout 30 0
 logging synchronous
line aux 0
line vty 0 4
 access-class 101 in
 exec-timeout 60 0
 logging synchronous
 login local
 transport input ssh
!
ntp server 10.0.1.200
!
end
EOF
}

show_cdp_neighbors() {
    cat <<'EOF'
Capability Codes: R - Router, T - Trans Bridge, B - Source Route Bridge
                  S - Switch, H - Host, I - IGMP, r - Repeater, P - Phone,
                  D - Remote, C - CVTA, M - Two-port Mac Relay

Device ID        Local Intrfce     Holdtme    Capability  Platform  Port ID
Switch1          Gig 0/1           142              S I   WS-C3750  Gig 1/0/1
Switch2          Gig 0/1           167              S I   WS-C2960  Gig 0/24
Router2          Gig 0/0           132              R     CISCO2911 Gig 0/0

Total cdp entries displayed : 3
EOF
}

show_env() {
    cat <<'EOF'
Number of Critical alarms:  0
Number of Major alarms:     1
Number of Minor alarms:     0

 Slot  Sensor         Current State   Reading        Threshold(Minor,Major,Critical,Shutdown)
 ----  ------         -------------   -------        ----------------------------------------
  0    Temp: Inlet    Normal          32 Celsius     (45 ,50 ,60 ,65 )(Celsius)
  0    Temp: Outlet   Warning         52 Celsius     (45 ,50 ,60 ,65 )(Celsius)
  0    Temp: CPU      Normal          41 Celsius     (75 ,85 ,95 ,100)(Celsius)
  P0   Pwr: PSU0      Normal          AC OK
  P1   Pwr: PSU1      Critical        AC FAIL
  0    Fan 0          Normal          5200 RPM
  0    Fan 1          Warning         2100 RPM       (2500,2000,1500,1000)(RPM)

Power
Supply  Model No         Type       Status
------  ----------------  ---------  -----------
PS0     PWR-2911-AC       AC         OK
PS1     PWR-2911-AC       AC         FAIL
EOF
}

show_spanning_tree() {
    cat <<'EOF'
VLAN0001
  Spanning tree enabled protocol ieee
  Root ID    Priority    32769
             Address     001e.f763.a800
             This bridge is the root
             Hello Time   2 sec  Max Age 20 sec  Forward Delay 15 sec

  Bridge ID  Priority    32769  (priority 32768 sys-id-ext 1)
             Address     001e.f763.a800
             Hello Time   2 sec  Max Age 20 sec  Forward Delay 15 sec
             Aging Time  300 sec

Interface           Role Sts Cost      Prio.Nbr Type
------------------- ---- --- --------- -------- ----------------
Gi0/1               Desg FWD 4         128.2    P2p
Gi0/2               Altn BLK 4         128.3    P2p
EOF
}

show_help() {
    cat <<'EOF'
Exec commands:
  enable          Turn on privileged commands
  exit            Exit from the EXEC
  ping            Send echo messages
  show            Show running system information
  terminal        Set terminal line parameters
  traceroute      Trace route to destination

show commands:
  access-lists       Access lists
  cdp                CDP information
  environment        Environmental monitor
  interfaces         Interface status and configuration
  ip bgp             BGP routing table
  ip bgp summary     BGP neighbor summary
  ip interface brief Interface brief summary
  ip ospf neighbor   OSPF neighbor table
  ip route           IP routing table
  logging            Logging buffer contents
  running-config     Current operating configuration
  spanning-tree      Spanning tree topology
  version            System hardware and software status
EOF
}

# Main loop
printf '\r\n'
printf 'Router1 con0 is now available\r\n'
printf '\r\n'
printf 'Press RETURN to get started.\r\n'
printf '\r\n'

while true; do
    prompt
    if ! IFS= read -r line; then
        break
    fi
    # Strip carriage returns (from SSH clients)
    line="${line%"$(printf '\r')"}"
    # Trim leading/trailing whitespace
    cmd=$(printf '%s' "$line" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')

    case "$cmd" in
        ""|" ")
            ;;
        "enable")
            ENABLE_MODE=1
            ;;
        "disable")
            ENABLE_MODE=0
            ;;
        "exit"|"quit"|"logout")
            printf 'Connection closed by foreign host.\r\n'
            exit 0
            ;;
        "show version"|"sh ver"*)
            show_version
            ;;
        "show interfaces"|"sh int"|"show int")
            show_interfaces
            ;;
        "show ip interface brief"|"sh ip int br"*|"show ip int br"*)
            show_ip_interface_brief
            ;;
        "show ip route"|"sh ip ro"*|"show ip ro"*)
            show_ip_route
            ;;
        "show ip bgp summary"|"sh ip bgp sum"*|"show ip bgp sum"*)
            show_ip_bgp_summary
            ;;
        "show ip bgp"|"sh ip bgp")
            show_ip_bgp
            ;;
        "show ip ospf neighbor"|"sh ip ospf ne"*|"show ip ospf ne"*)
            show_ip_ospf_neighbor
            ;;
        "show access-lists"|"sh access"*|"show access"*)
            show_access_lists
            ;;
        "show logging"|"sh log"*|"show log"*)
            show_logging
            ;;
        "show running-config"|"sh run"*|"show run"*)
            show_running_config
            ;;
        "show cdp neighbors"|"sh cdp ne"*|"show cdp ne"*)
            show_cdp_neighbors
            ;;
        "show environment"|"sh env"*|"show env"*)
            show_env
            ;;
        "show spanning-tree"|"sh span"*|"show span"*)
            show_spanning_tree
            ;;
        "?"|"help")
            show_help
            ;;
        "configure terminal"|"conf t")
            if [ "$ENABLE_MODE" -eq 1 ]; then
                printf 'Enter configuration commands, one per line. End with CNTL/Z.\r\n'
                printf '%s(config)# ' "$HOSTNAME"
                read -r _
            else
                printf '           ^^\r\n'
                printf "%% Invalid input detected at '^' marker.\r\n"
            fi
            ;;
        "ping "*)
            target="${cmd#ping }"
            printf 'Type escape sequence to abort.\r\n'
            printf 'Sending 5, 100-byte ICMP Echos to %s, timeout is 2 seconds:\r\n' "$target"
            printf '!!!!!\r\n'
            printf 'Success rate is 100 percent (5/5), round-trip min/avg/max = 1/2/4 ms\r\n'
            ;;
        "traceroute "*)
            target="${cmd#traceroute }"
            printf 'Type escape sequence to abort.\r\n'
            printf 'Tracing the route to %s\r\n' "$target"
            printf 'VRF info: (vrf in name/id, vrf out name/id)\r\n'
            printf '  1 203.0.113.254  1 msec  1 msec  1 msec\r\n'
            printf '  2 198.51.100.1   4 msec  3 msec  4 msec\r\n'
            printf '  3 %s  8 msec  7 msec  8 msec\r\n' "$target"
            ;;
        "terminal length "*)
            printf '  %% Simulated - no effect.\r\n'
            ;;
        "show"*)
            printf '           ^^\r\n'
            printf "%% Invalid input detected at '^' marker.\r\n"
            printf "Type 'show ?' for a list of show commands.\r\n"
            ;;
        *)
            printf '           ^^\r\n'
            printf "%% Invalid input detected at '^' marker.\r\n"
            ;;
    esac
done
